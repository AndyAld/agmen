//! Wire codec and shared types for the local API between agmend and the
//! desktop: length-prefixed JSON frames over a Unix socket.
//!
//! The default build is blocking `std` I/O with no tokio, for the desktop.
//! Feature `tokio` adds async `read_async` and `write_async` for agmend.

#![warn(missing_docs)]

pub mod console;
pub mod frame;
mod message;
pub mod roles;
pub mod safeguards;

pub use message::{API, ApiError, ErrorCode, HelloAnswer, Outcome, ReadyAnswer, Request, Response};

/// The largest frame body either side accepts or sends, in bytes.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// The idle interval between identity watch keepalive frames. agmend sends one
/// after this long without a frame; the CLI reconnects after twice this long.
pub const WATCH_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);
