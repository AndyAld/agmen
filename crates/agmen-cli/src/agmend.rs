//! Talks to a running agmend over its Unix socket.
//!
//! One connection sends `hello` once, then human calls and identity-scoped
//! calls in order. Worker commands call it; listener commands will follow.

// shortcut: Windows named pipes replace this Unix socket when S6 lands (D36).
#![cfg(unix)]

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use agmen_local::frame::{read_async, write_async};
use agmen_local::{API, ErrorCode, HelloAnswer, Outcome, Request, Response};
use agmen_proto::IdentityId;
use serde_json::{Value, json};
use tokio::net::UnixStream;

/// How long `connect` may spend reaching the socket and finishing `hello`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long one `call` or `call_as` waits for agmend's answer.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One connection to agmend. Request ids start at 1 for `hello` and then
/// increase by one. A local token is never stored here.
pub(crate) struct Agmend {
    stream: Option<UnixStream>,
    hello: HelloAnswer,
    next_id: u64,
    call_timeout: Duration,
}

impl std::fmt::Debug for Agmend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agmend")
            .field("hello", &self.hello)
            .field("next_id", &self.next_id)
            .field("connected", &self.stream.is_some())
            .finish_non_exhaustive()
    }
}

impl Agmend {
    /// Connects to `socket`, sends `hello {api: API}`, and keeps the answer.
    ///
    /// The socket connect and hello together wait at most 2 seconds.
    ///
    /// # Errors
    ///
    /// [`AgmendError::NotRunning`] when the socket is missing or nothing is
    /// listening, [`AgmendError::Version`] when agmend speaks another local
    /// API, [`AgmendError::Timeout`] when the 2 second budget runs out, and
    /// [`AgmendError::Protocol`] or [`AgmendError::Frame`] when the hello
    /// exchange is not a usable answer.
    pub(crate) async fn connect(socket: &Path) -> Result<Self, AgmendError> {
        open(socket, CALL_TIMEOUT).await
    }

    /// Connects to [`agmen_client::agmend_socket`].
    ///
    /// Checks ownership and mode before connecting. CLI tests isolate the home.
    ///
    /// # Errors
    ///
    /// As [`connect`](Self::connect), plus [`AgmendError::Io`] when the state
    /// folder cannot be named.
    pub(crate) async fn connect_default() -> Result<Self, AgmendError> {
        let path = agmen_client::agmend_socket()
            .map_err(|err| AgmendError::Io(std::io::Error::other(err)))?;
        agmen_client::check_agmend_socket(&path).map_err(|error| match error {
            agmen_client::AgmendSocketError::Missing => AgmendError::NotRunning,
            other => AgmendError::Socket(other),
        })?;
        Self::connect(&path).await
    }

    /// [`connect`](Self::connect) with `call_timeout` in place of the 30 second
    /// call budget. Hello still uses the 2 second connect budget.
    ///
    /// # Errors
    ///
    /// As [`connect`](Self::connect).
    #[cfg(test)]
    pub(crate) async fn connect_with_call_timeout(
        socket: &Path,
        call_timeout: Duration,
    ) -> Result<Self, AgmendError> {
        open(socket, call_timeout).await
    }

    /// The `hello` answer stored at connect.
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code, reason = "used by L21-CLI"))]
    pub(crate) fn hello(&self) -> &HelloAnswer {
        &self.hello
    }

    /// Sends one human call and waits up to 30 seconds (or the test budget).
    ///
    /// # Errors
    ///
    /// [`AgmendError::Timeout`] when agmend does not answer in time,
    /// [`AgmendError::Refused`] when agmend returns an error (whatever its
    /// code, including `Version`, which `ready` uses for a store schema agmend
    /// cannot read), and [`AgmendError::Protocol`] or [`AgmendError::Frame`]
    /// when the frames are not a matching answer. A timeout, a frame error, or
    /// a mismatched id closes the socket. A refusal leaves it open.
    pub(crate) async fn call(&mut self, method: &str, params: Value) -> Result<Value, AgmendError> {
        let request = Request::new(self.alloc_id()?, method, params);
        exchange(&mut self.stream, request, self.call_timeout).await
    }

    /// Sends one identity-scoped call: `as` is `identity`, and `local_token`
    /// is added to `params`.
    ///
    /// `params` must be a JSON object. Otherwise this returns
    /// [`AgmendError::BadParams`] and sends nothing.
    ///
    /// # Errors
    ///
    /// [`AgmendError::BadParams`] when `params` is not an object, and the same
    /// connection errors as [`call`](Self::call).
    #[cfg_attr(not(test), expect(dead_code, reason = "used by L21-CLI"))]
    pub(crate) async fn call_as(
        &mut self,
        identity: &IdentityId,
        token: &LocalToken,
        method: &str,
        params: Value,
    ) -> Result<Value, AgmendError> {
        let Value::Object(mut params) = params else {
            return Err(AgmendError::BadParams);
        };
        // The token moves into the frame and is not written anywhere else.
        params.insert("local_token".to_owned(), Value::String(token.0.clone()));
        let mut request = Request::new(self.alloc_id()?, method, Value::Object(params));
        request.as_ = Some(identity.clone());
        exchange(&mut self.stream, request, self.call_timeout).await
    }

    /// The next request id. Fails instead of wrapping, so two calls never share an id.
    fn alloc_id(&mut self) -> Result<u64, AgmendError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            AgmendError::Protocol("agmend request ids are exhausted. Connect again".into())
        })?;
        Ok(id)
    }
}

/// A local token: 64 lowercase hex digits. `Debug` prints
/// `LocalToken(<redacted>)` and there is no `Display`.
pub(crate) struct LocalToken(String);

impl std::fmt::Debug for LocalToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalToken(<redacted>)")
    }
}

impl FromStr for LocalToken {
    type Err = AgmendError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hex = text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if hex {
            Ok(Self(text.to_owned()))
        } else {
            Err(AgmendError::Protocol(
                "a local token is 64 lowercase hex digits".into(),
            ))
        }
    }
}

/// Why a connect or a call did not succeed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AgmendError {
    /// The default socket failed the shared ownership or mode check.
    #[error("{0}")]
    Socket(#[source] agmen_client::AgmendSocketError),
    /// The socket is missing, or nothing is listening.
    #[error("agmend is not running: open the agmen app or run agmend service install")]
    NotRunning,
    /// agmend's answer to hello shows a different local API than this client.
    #[error(
        "agmend speaks local API {daemon}; this client speaks {client}. Update the older one so both speak the same local API"
    )]
    Version {
        /// The API version agmend named, or 0 when the refusal did not name one.
        daemon: u32,
        /// The API version this process speaks (`agmen_local::API`).
        client: u32,
    },
    /// The wait ended before agmend answered.
    #[error("agmend did not answer `{method}` in time. Check that agmend is running and try again")]
    Timeout {
        /// `connect`, or the method that was waiting.
        method: String,
    },
    /// agmend returned an error frame, with agmend's own code and message.
    /// Hello's `Version` code becomes [`AgmendError::Version`]; on any other
    /// call it stays here.
    #[error("agmend refused the call ({code:?}): {message}")]
    Refused {
        /// agmend's error code.
        code: ErrorCode,
        /// agmend's own message.
        message: String,
    },
    /// The frames were out of step, or a value was not one this client speaks.
    #[error("{0}")]
    Protocol(String),
    /// `call_as` was given params that are not a JSON object, so nothing was sent.
    #[error("call_as params must be a JSON object; the local token is added to that object")]
    BadParams,
    /// The socket could not be used. The source is the OS error.
    #[error("agmend socket failed")]
    Io(#[source] std::io::Error),
    /// A frame could not be read or written. The source is the frame error.
    #[error("agmend frame failed")]
    Frame(#[source] agmen_local::frame::FrameError),
}

/// Connects, says hello, and builds the client only after the answer checks out.
async fn open(socket: &Path, call_timeout: Duration) -> Result<Agmend, AgmendError> {
    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    let mut stream = Some(connect_stream(socket, deadline).await?);
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let result = exchange(
        &mut stream,
        Request::new(1, "hello", json!({ "api": API })),
        remaining,
    )
    .await
    .map_err(|err| match err {
        // Only hello's Version code means the two sides speak different local
        // APIs. After hello, agmend also uses it for a store schema it cannot
        // read (`ready`), so a call keeps agmend's message as a refusal.
        AgmendError::Refused {
            code: ErrorCode::Version,
            message,
        } => version_mismatch(&message),
        other => other,
    })?;
    Ok(Agmend {
        stream,
        hello: parse_hello(result)?,
        next_id: 2,
        call_timeout,
    })
}

async fn connect_stream(
    socket: &Path,
    deadline: tokio::time::Instant,
) -> Result<UnixStream, AgmendError> {
    match tokio::time::timeout_at(deadline, UnixStream::connect(socket)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(err)) => Err(map_connect(err)),
        Err(_elapsed) => Err(AgmendError::Timeout {
            method: "connect".into(),
        }),
    }
}

fn map_connect(err: std::io::Error) -> AgmendError {
    match err.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            AgmendError::NotRunning
        }
        _ => AgmendError::Io(err),
    }
}

/// Writes one request and reads its response.
///
/// On timeout, a frame error, or a mismatched id the socket is dropped.
/// `read_async` is not cancel safe, and a bad id means the stream is out of
/// step, so the next call fails until the caller connects again.
async fn exchange(
    link: &mut Option<UnixStream>,
    request: Request,
    budget: Duration,
) -> Result<Value, AgmendError> {
    let id = request.id;
    let method = request.method.clone();
    let mut current = link.take().ok_or_else(|| {
        AgmendError::Protocol(
            "the connection to agmend closed after an error. Connect again".into(),
        )
    })?;
    let outcome = tokio::time::timeout(budget, async {
        if let Err(err) = write_async(&mut current, &request).await {
            return (current, Err(AgmendError::Frame(err)));
        }
        match read_async::<Response>(&mut current).await {
            Ok(response) => (current, Ok(response)),
            Err(err) => (current, Err(AgmendError::Frame(err))),
        }
    })
    .await;

    let response = match outcome {
        Ok((current, Ok(response))) => {
            if response.id != id {
                return Err(AgmendError::Protocol(format!(
                    "agmend answered id {} for {method}, which was sent as id {id}. Connect again",
                    response.id
                )));
            }
            *link = Some(current);
            response
        }
        Ok((_current, Err(err))) => return Err(err),
        Err(_elapsed) => {
            return Err(AgmendError::Timeout { method });
        }
    };
    match response.outcome {
        Outcome::Ok { result } => Ok(result),
        Outcome::Err { error } => Err(AgmendError::Refused {
            code: error.code,
            message: error.message,
        }),
    }
}

fn version_mismatch(message: &str) -> AgmendError {
    let daemon = message
        .split(|ch: char| !ch.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    AgmendError::Version {
        daemon,
        client: API,
    }
}

fn parse_hello(result: Value) -> Result<HelloAnswer, AgmendError> {
    let hello: HelloAnswer = serde_json::from_value(result).map_err(|err| {
        AgmendError::Protocol(format!(
            "agmend's hello answer was not a version report ({err}). Update agmend"
        ))
    })?;
    if hello.api != API {
        return Err(AgmendError::Version {
            daemon: hello.api,
            client: API,
        });
    }
    Ok(hello)
}

#[cfg(test)]
#[path = "agmend_tests.rs"]
mod tests;
