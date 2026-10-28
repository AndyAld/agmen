//! One local API connection: `hello` first, then requests one at a time,
//! each answered before the next is read (m0-m1 Task 13 frames).
//!
//! Each request authenticates its own identity credential before dispatch.

use std::sync::Arc;

use agmen_local::{API, ApiError, ErrorCode, MAX_FRAME, Request, Response, frame};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::Core;
use crate::console::{Console, bad_request, params};
use crate::identities::{Caller, forbidden, unauthorized};
use agmen_proto::{ClientEnvelope, ProjectId};

/// `hello`'s params.
#[derive(Debug, Deserialize)]
struct Hello {
    api: u32,
}

/// Serves `stream` until the peer hangs up or breaks the framing.
pub(crate) async fn serve(stream: UnixStream, core: Arc<Core>) {
    // shortcut: Unix-only local API; revisit when S6 Windows IPC is approved.
    let console = &core.console;
    let (mut reader, mut writer) = stream.into_split();
    let mut greeted = false;
    loop {
        let mut request: Request = match frame::read_async(&mut reader).await {
            Ok(request) => request,
            Err(frame::FrameError::Io(_)) => return,
            Err(err) => {
                // The id is unreadable, so the answer carries 0, and the
                // stream may be out of step, so it closes.
                let answer = Response::err(0, ErrorCode::BadRequest, err.to_string());
                if let Err(err) = frame::write_async(&mut writer, &answer).await {
                    tracing::debug!(error = %err, "a client left before its error");
                }
                return;
            }
        };
        let id = request.id;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let bounded = matches!(request.method.as_str(), "inbox" | "state" | "pending");
        let outcome = if let Some(refusal) = refuse(&request, greeted) {
            Err(refusal)
        } else {
            let token = extract_token(&mut request.params);
            let caller = match token {
                Ok(ref token) => {
                    core.identities
                        .authorize(request.as_.as_ref(), token.as_deref())
                        .await
                }
                Err(error) => Err(error),
            };
            match caller {
                Err(error) => Err(error),
                Ok(caller) if request.method == "watch" => {
                    if let Err(error) = core
                        .identities
                        .watch(&caller, id, request.params, reader, &mut writer)
                        .await
                    {
                        // A terminal error is best effort; a blocked reader is closed.
                        let _written = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            answer(&mut writer, id, Err(error)),
                        )
                        .await;
                    }
                    return;
                }
                Ok(caller) if request.method == "inbox" => {
                    finite(deadline, core.identities.inbox(&caller, request.params)).await
                }
                Ok(caller) if request.method == "state" => {
                    finite(deadline, core.identities.state(&caller, request.params)).await
                }
                Ok(caller) if request.method == "pending" => {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Pending {}
                    match params::<Pending>(request.params) {
                        Err(error) => Err(error),
                        Ok(_) => match caller {
                            Caller::Human => {
                                Err(bad_request("pending requires an identity caller"))
                            }
                            Caller::Identity(identity) => {
                                finite(deadline, core.identities.pending(&identity)).await
                            }
                        },
                    }
                }
                Ok(Caller::Identity(caller)) => {
                    identity_call(&core, &caller, &request.method, request.params).await
                }
                Ok(Caller::Human) if request.method == "hello" => {
                    hello(request.params).inspect(|_| greeted = true)
                }
                Ok(Caller::Human) if request.method == "console_watch" => {
                    // The watch keeps the connection; whatever it ends with is the
                    // last frame.
                    if let Err(error) = console.watch(id, request.params, reader, &mut writer).await
                    {
                        answer(&mut writer, id, Err(error)).await;
                    }
                    return;
                }
                Ok(Caller::Human) => call(console, &request.method, request.params).await,
            }
        };
        let written = if bounded {
            tokio::time::timeout_at(deadline, answer(&mut writer, id, outcome))
                .await
                .unwrap_or(false)
        } else {
            answer(&mut writer, id, outcome).await
        };
        if !written {
            return;
        }
    }
}

async fn finite(
    deadline: tokio::time::Instant,
    operation: impl std::future::Future<Output = Result<Value, ApiError>>,
) -> Result<Value, ApiError> {
    tokio::time::timeout_at(deadline, operation)
        .await
        .map_err(|_| ApiError {
            code: ErrorCode::Hub,
            message: "local operation timed out".into(),
        })?
}

/// Why `request` cannot run, if it cannot.
fn refuse(request: &Request, greeted: bool) -> Option<ApiError> {
    if request.v != API {
        return Some(ApiError {
            code: ErrorCode::Version,
            message: format!(
                "agmend speaks local API {API}; this request is v {}",
                request.v
            ),
        });
    }
    if !greeted && request.method != "hello" {
        return Some(bad_request("the first request must be hello {api: 1}"));
    }
    if request.method == "hello"
        && let Err(error) = hello(request.params.clone())
    {
        return Some(error);
    }
    None
}

fn extract_token(raw: &mut Value) -> Result<Option<String>, ApiError> {
    match raw
        .as_object_mut()
        .and_then(|object| object.remove("local_token"))
    {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(unauthorized()),
    }
}

async fn identity_call(
    core: &Core,
    caller: &crate::state::ManagedIdentity,
    method: &str,
    raw: Value,
) -> Result<Value, ApiError> {
    match method {
        "post" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Post {
                #[serde(default)]
                project: Option<ProjectId>,
                envelope: ClientEnvelope,
            }
            let Post { project, envelope } = params(raw)?;
            if project.is_some_and(|project| project != caller.project) {
                return Err(forbidden());
            }
            core.identities.post(caller, envelope).await
        }
        "hello" | "whoami" | "console_watch" | "console_snapshot" | "chat_history"
        | "file_text" | "identity_mint" | "identity_list" => Err(forbidden()),
        other => Err(ApiError {
            code: ErrorCode::NotFound,
            message: format!("agmend has no method {other:?}"),
        }),
    }
}

fn hello(raw: Value) -> Result<Value, ApiError> {
    let Hello { api } = params(raw)?;
    if api == API {
        Ok(json!({ "api": API }))
    } else {
        Err(ApiError {
            code: ErrorCode::Version,
            message: format!("agmend speaks local API {API}; this client asked for {api}"),
        })
    }
}

async fn call(console: &Console, method: &str, raw: Value) -> Result<Value, ApiError> {
    match method {
        "whoami" => console.whoami().await,
        "console_snapshot" => console.snapshot(raw).await,
        "chat_history" => console.history(raw).await,
        "post" => console.post(raw).await,
        "file_text" => console.file_text(raw).await,
        other => Err(ApiError {
            code: ErrorCode::NotFound,
            message: format!("agmend has no method {other:?}"),
        }),
    }
}

/// Writes the answer; false when the client is gone.
async fn answer(writer: &mut OwnedWriteHalf, id: u64, outcome: Result<Value, ApiError>) -> bool {
    let response = match outcome {
        Ok(result) => Response::ok(id, result),
        Err(error) => Response::err(id, error.code, error.message),
    };
    match frame::write_async(writer, &response).await {
        Ok(()) => true,
        // Nothing was written, so the stream is still in step: say so
        // instead of hanging up, or the client would ask again forever.
        Err(frame::FrameError::TooLarge { len }) => {
            let refusal = Response::err(
                id,
                ErrorCode::Hub,
                format!("the answer is {len} bytes, over the {MAX_FRAME} byte frame cap"),
            );
            frame::write_async(writer, &refusal).await.is_ok()
        }
        Err(err) => {
            tracing::debug!(error = %err, "writing an answer failed");
            false
        }
    }
}
