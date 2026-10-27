use agmen_proto::IdentityId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The local API version in every request's `v`.
pub const API: u32 = 1;

/// A request frame: `{ "v", "id", "method", "params", "as"? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// The API version the caller speaks.
    pub v: u32,
    /// Chosen by the caller; the response echoes it.
    pub id: u64,
    /// The method name.
    pub method: String,
    /// Method arguments.
    #[serde(default)]
    pub params: Value,
    /// The identity to act as; left out when the caller uses its default.
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub as_: Option<IdentityId>,
}

impl Request {
    /// A request at [`API`] version acting as the default identity.
    #[must_use]
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            v: API,
            id,
            method: method.into(),
            params,
            as_: None,
        }
    }
}

/// Why a request failed.
///
/// A code this build does not know fails to parse; the daemon and the desktop
/// ship together, so a skewed pair is reported by `version` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request's `v` is not supported.
    Version,
    /// The project, identity or method does not exist.
    NotFound,
    /// The identity may not do this.
    Unauthorized,
    /// The hub refused or could not be reached.
    Hub,
    /// The params are malformed.
    BadRequest,
}

/// The `error` member of a failed [`Response`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// What kind of failure.
    pub code: ErrorCode,
    /// Text for a person.
    pub message: String,
}

/// A response frame: `{ "id", "result" }` or `{ "id", "error" }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The request's `id`.
    pub id: u64,
    /// Set on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Set on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Response {
    /// A success carrying `result`.
    #[must_use]
    pub const fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    /// A failure.
    #[must_use]
    pub fn err(id: u64, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(ApiError {
                code,
                message: message.into(),
            }),
        }
    }
}
