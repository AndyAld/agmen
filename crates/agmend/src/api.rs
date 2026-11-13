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
use crate::console::{bad_request, params};
use crate::identities::{Caller, forbidden, unauthorized};
use agmen_proto::{ClientEnvelope, ProjectId};

/// `hello`'s params.
#[derive(Debug, Deserialize)]
struct Hello {
    api: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shutdown {}

const REQUEST_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const BOUNDED_METHODS: [&str; 12] = [
    "inbox",
    "state",
    "pending",
    "ready",
    "shutdown",
    "host_status",
    "host_restart",
    "page",
    "lanes",
    "items",
    "check_path",
    "local_mint",
];

/// Serves `stream` until the peer hangs up or breaks the framing.
pub(crate) async fn serve(stream: UnixStream, core: Arc<Core>) {
    serve_with_peer(stream, core, |stream| {
        stream
            .peer_cred()?
            .pid()
            .ok_or_else(|| std::io::Error::other("peer PID unavailable"))
    })
    .await;
}

pub(crate) async fn serve_with_peer(
    stream: UnixStream,
    core: Arc<Core>,
    peer: impl FnOnce(&UnixStream) -> std::io::Result<i32>,
) {
    let peer = peer(&stream);
    let mut groups = core.managed.peer_groups();
    groups.extend(core.listeners.peer_groups());
    let managed = peer
        .as_ref()
        .is_ok_and(|pid| core.managed.contains_peer(*pid));
    // The ancestry answer belongs to this connection, including its watch. Its walk waits
    // for the first request that needs it, so `hello` never does.
    let mut peer = crate::peer::Peer::new(peer, groups, managed);
    core.managed.attach(&core.identities).await;
    core.listeners.attach(&core.identities, &core.managed).await;
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
        let deadline = tokio::time::Instant::now() + REQUEST_WAIT;
        let bounded = BOUNDED_METHODS.contains(&request.method.as_str());
        let mut shutting_down = false;
        let outcome = if let Some(refusal) = refuse(&request, greeted) {
            Err(refusal)
        } else {
            let caller = authorize(&core, &mut request, &mut peer).await;
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
                    finite(deadline, pending(&core, caller, request.params)).await
                }
                Ok(Caller::Identity(caller)) => {
                    identity_call(&core, &caller, &request.method, request.params, deadline).await
                }
                Ok(Caller::Human) if request.method == "hello" => {
                    hello(request.params).inspect(|_| greeted = true)
                }
                Ok(Caller::Human) if request.method == "shutdown" => {
                    let result = params::<Shutdown>(request.params).map(|_| json!({}));
                    shutting_down = result.is_ok();
                    result
                }
                Ok(Caller::Human) if request.method == "console_watch" => {
                    core.managed.remember_root(&request.params).await;
                    // The watch keeps the connection; whatever it ends with is the
                    // last frame.
                    if let Err(error) = console.watch(id, request.params, reader, &mut writer).await
                    {
                        answer(&mut writer, id, Err(error)).await;
                    }
                    return;
                }
                Ok(Caller::Human) => call(&core, &request.method, request.params, deadline).await,
            }
        };
        let written = if bounded {
            tokio::time::timeout_at(deadline, answer(&mut writer, id, outcome))
                .await
                .unwrap_or(false)
        } else {
            answer(&mut writer, id, outcome).await
        };
        if shutting_down {
            core.shutdown.notify_one();
            return;
        }
        if !written {
            return;
        }
    }
}

#[cfg(test)]
pub(crate) fn owner_peer_check(
    managed: &crate::managed::Managed,
    caller: &Caller,
    method: &str,
    lookup: impl FnOnce() -> std::io::Result<i32>,
) -> Result<(), ApiError> {
    if needs_owner_peer(caller, method) && !lookup().is_ok_and(|pid| !managed.contains_peer(pid)) {
        Err(owner_only())
    } else {
        Ok(())
    }
}

/// Whether `method` from `caller` is for the person's own process only, so it waits for
/// the peer's ancestry walk.
fn needs_owner_peer(caller: &Caller, method: &str) -> bool {
    // Identity-scoped chat uses its authenticated identity, never the human post.
    if (matches!(method, "post" | "child_launch" | "child_stop")
        && matches!(caller, Caller::Identity(_)))
        || (matches!(caller, Caller::Identity(_))
            && matches!(
                method,
                "git_log"
                    | "git_show"
                    | "git_diff"
                    | "git_status"
                    | "settings_get"
                    | "settings_apply"
                    | "roster"
                    | "page"
                    | "lanes"
                    | "items"
                    | "check_path"
                    | "inbox"
                    | "state"
                    | "watch"
            ))
        // Without an identity, inbox, state and watch answer the person, on any origin, so
        // they need the owner peer like the other owner methods.
        || matches!(method, "hello" | "pending")
    {
        return false;
    }
    true
}

fn owner_only() -> ApiError {
    ApiError {
        code: ErrorCode::Forbidden,
        message: "agents cannot call owner-only methods".into(),
    }
}

async fn pending(core: &Core, caller: Caller, raw: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pending {}
    params::<Pending>(raw)?;
    match caller {
        Caller::Human => Err(bad_request("pending requires an identity caller")),
        Caller::Identity(identity) if identity.kind == agmen_proto::IdentityKind::Listener => {
            Err(ApiError {
                code: ErrorCode::Forbidden,
                message: "the listener's messages arrive in its batches".into(),
            })
        }
        Caller::Identity(identity) => core.identities.pending(&identity).await,
    }
}

async fn ready(core: &Core, raw: Value, deadline: tokio::time::Instant) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ready {}
    params::<Ready>(raw)?;
    let permit = core
        .schema_scan
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| bad_request("store inspection stopped"))?;
    let dir = core.state_dir.clone();
    let scan_deadline = deadline.into_std();
    let schemas = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::state::schemas(&dir, scan_deadline)
    })
    .await
    .map_err(|_| bad_request("reading store schemas failed"))?
    .map_err(|error| ApiError {
        code: ErrorCode::Version,
        message: format!("{error:#}"),
    })?;
    Ok(json!(agmen_local::ReadyAnswer {
        ready: true,
        schemas
    }))
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

async fn authorize(
    core: &Core,
    request: &mut Request,
    peer: &mut crate::peer::Peer,
) -> Result<Caller, ApiError> {
    let token = match request
        .params
        .as_object_mut()
        .and_then(|object| object.remove("local_token"))
    {
        None => None,
        Some(Value::String(value)) => Some(value),
        Some(_) => return Err(unauthorized()),
    };
    let caller = core
        .identities
        .authorize(request.as_.as_ref(), token.as_deref())
        .await?;
    if needs_owner_peer(&caller, &request.method) && !peer.is_owner(&core.peers).await {
        return Err(owner_only());
    }
    Ok(caller)
}

async fn identity_call(
    core: &Core,
    caller: &crate::state::ManagedIdentity,
    method: &str,
    raw: Value,
    deadline: tokio::time::Instant,
) -> Result<Value, ApiError> {
    match method {
        "child_launch" => core.managed.launch_child(caller, raw).await,
        "child_stop" => core.managed.stop_child(caller, raw).await,
        "page" | "lanes" | "items" | "check_path" => {
            finite(deadline, core.identities.read_local(caller, method, raw)).await
        }
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
            if caller.kind == agmen_proto::IdentityKind::Listener {
                core.listeners.post(caller, envelope).await
            } else {
                core.identities.post(caller, envelope).await
            }
        }
        "git_log" | "git_show" | "git_diff" | "git_status" | "settings_get" | "settings_apply"
        | "roster" => core.listeners.identity_call(caller, method, raw).await,
        "shutdown"
        | "hello"
        | "ready"
        | "whoami"
        | "console_watch"
        | "console_snapshot"
        | "chat_history"
        | "file_text"
        | "identity_mint"
        | "identity_list"
        | "agent_launch"
        | "host_status"
        | "host_restart"
        | "agent_stop"
        | "agent_list"
        | "agent_providers"
        | "agent_start"
        | "agent_attach"
        | "agent_detach"
        | "agent_remove"
        | "agent_budget"
        | "prune"
        | "login_start"
        | "login_status"
        | "listener_start"
        | "listener_stop"
        | "listener_status"
        | "listener_set"
        | "settings_decide"
        | "settings_undo"
        | "local_project_create"
        | "local_projects"
        | "local_mint"
        | "lanes_set" => Err(forbidden()),
        other => Err(ApiError {
            code: ErrorCode::NotFound,
            message: format!("agmend has no method {other:?}"),
        }),
    }
}

fn hello(raw: Value) -> Result<Value, ApiError> {
    let Hello { api } = params(raw)?;
    if api == API {
        let exe = std::env::current_exe()
            .map_err(|error| bad_request(format!("locating running executable: {error}")))?;
        Ok(json!(agmen_local::HelloAnswer {
            api: API,
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            build: Some(option_env!("AGMEN_BUILD").unwrap_or("dev").to_owned()),
            exe: Some(exe),
        }))
    } else {
        Err(ApiError {
            code: ErrorCode::Version,
            message: format!("agmend speaks local API {API}; this client asked for {api}"),
        })
    }
}

async fn call(
    core: &Arc<Core>,
    method: &str,
    raw: Value,
    deadline: tokio::time::Instant,
) -> Result<Value, ApiError> {
    let console = &core.console;
    match method {
        "local_mint" => finite(deadline, core.identities.local_mint(&core.managed, raw)).await,
        "local_project_create" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Create {
                name: String,
            }
            let Create { name } = params(raw)?;
            let hint = match core.origins.hub().credentials().login().await {
                Ok(login) => login.handle,
                Err(agmen_client::LoginError::NotSignedIn) => {
                    std::env::var("USER").unwrap_or_default()
                }
                Err(error) => return Err(crate::hub::HubError::from(error).into()),
            };
            let local = core.origins.local();
            let topics = local.create_project(&name, &hint).await?;
            let person = local
                .person()
                .await?
                .ok_or_else(|| bad_request("local person is unavailable"))?;
            Ok(
                json!({"project": topics.project(), "tenant": topics.tenant(), "person": {"id": person.id, "handle": person.handle}}),
            )
        }
        "local_projects" => {
            params::<Shutdown>(raw)?;
            let local = core.origins.local();
            let person = local.person().await?;
            let projects = if person.is_some() {
                local.projects().await?
            } else {
                Vec::new()
            };
            let person = person.map(|person| json!({"id": person.id, "handle": person.handle}));
            Ok(
                json!({"projects": projects.into_iter().map(|(project, name)| json!({"project": project, "name": name})).collect::<Vec<_>>(), "person": person}),
            )
        }
        "lanes_set" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Set {
                project: ProjectId,
                lanes: Vec<agmen_core::LaneSpec>,
            }
            let Set { project, lanes } = params(raw)?;
            serde_json::to_value(core.origins.local().replace_lanes(&project, lanes).await?)
                .map_err(|error| bad_request(error.to_string()))
        }
        "host_status" => params::<Shutdown>(raw).map(|_| core.host.status()),
        "host_restart" => params::<Shutdown>(raw).and_then(|_| {
            core.host
                .restart()
                .map_err(|error| bad_request(format!("{error:#}")))
        }),
        "ready" => finite(deadline, ready(core, raw, deadline)).await,
        "agent_launch" => core.managed.launch(raw).await,
        "agent_stop" => core.managed.stop(raw).await,
        "agent_list" => core.managed.list(raw).await,
        "agent_providers" => core.managed.providers().await,
        "agent_start" => core.managed.start(raw).await,
        "agent_attach" => core.managed.attach_claude(raw).await,
        "agent_detach" => core.managed.detach(raw).await,
        "agent_remove" => core.managed.remove(raw).await,
        "agent_budget" => core.managed.agent_budget(raw).await,
        "prune" => core.managed.prune(raw).await,
        "login_start" => console.hub.credentials().login_start(raw).await,
        "login_status" => console.hub.credentials().login_status(raw).await,
        "listener_start" | "listener_stop" | "listener_status" | "listener_set"
        | "settings_decide" | "settings_undo" => core.listeners.human_call(method, raw).await,
        "whoami" => console.whoami().await,
        "console_snapshot" => {
            core.managed.remember_root(&raw).await;
            console.snapshot(raw).await
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn finite_identity_read_timeout_preserves_spent_refresh() {
        let fixture = crate::login::tests::RefreshFixture::new().await;
        let core = fixture.daemon.core.clone();
        let read = tokio::spawn(async move {
            finite(
                // Expire before the identity read's own 10 s timeout.
                tokio::time::Instant::now() + Duration::from_secs(1),
                core.identities.state(
                    &Caller::Human,
                    json!({"project": "project1", "key": "head"}),
                ),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), fixture.refresh.started.notified())
            .await
            .expect("the hub must spend the refresh token before the timeout");
        let error = tokio::time::timeout(Duration::from_secs(3), read)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Hub);
        assert_eq!(error.message, "local operation timed out");
        fixture.finish_refresh().await;
    }
}
