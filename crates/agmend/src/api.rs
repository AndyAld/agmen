//! One local API connection: `hello` first, then requests one at a time,
//! each answered before the next is read (m0-m1 Task 13 frames).
//!
//! Requests act as the person. A request with `as` is refused with
//! `bad_request` for now: identities act through agmend over MQTT in m0-m1
//! Task 13, which this crate does not build yet.

use std::sync::Arc;

use agmen_local::{API, ApiError, ErrorCode, MAX_FRAME, Request, Response, frame};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::Core;
use crate::console::{Console, bad_request, params};

/// `hello`'s params.
#[derive(Debug, Deserialize)]
struct Hello {
    api: u32,
}

/// Serves `stream` until the peer hangs up or breaks the framing.
pub(crate) async fn serve(stream: UnixStream, core: Arc<Core>) {
    let console = &core.console;
    let (mut reader, mut writer) = stream.into_split();
    let mut greeted = false;
    loop {
        let request: Request = match frame::read_async(&mut reader).await {
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
        let outcome = if let Some(refusal) = refuse(&request, greeted) {
            Err(refusal)
        } else if request.method == "hello" {
            hello(request.params).inspect(|_| greeted = true)
        } else if request.method == "console_watch" {
            // The watch keeps the connection; whatever it ends with is the
            // last frame.
            if let Err(error) = console.watch(id, request.params, reader, &mut writer).await {
                answer(&mut writer, id, Err(error)).await;
            }
            return;
        } else {
            call(console, &request.method, request.params).await
        };
        if !answer(&mut writer, id, outcome).await {
            return;
        }
    }
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
    if request.as_.is_some() {
        return Some(bad_request(
            "acting as an identity (as) is not supported yet; leave it out to act as yourself",
        ));
    }
    None
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
