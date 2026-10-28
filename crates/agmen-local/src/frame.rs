//! Frames: a 4-byte big-endian length, then that many bytes of UTF-8 JSON.
//! Bodies over [`MAX_FRAME`] are refused on both read and write.

use std::io::{Read, Write};

use serde::{Serialize, de::DeserializeOwned};

use crate::MAX_FRAME;

/// Why a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The stream failed or ended mid-frame.
    #[error("frame i/o failed")]
    Io(#[from] std::io::Error),
    /// The body is not the expected JSON.
    #[error("frame body is not valid JSON for this type")]
    Json(#[from] serde_json::Error),
    /// The length is over [`MAX_FRAME`].
    #[error("frame of {len} bytes is over the {MAX_FRAME} byte cap")]
    TooLarge {
        /// The length the frame declared or would have had.
        len: u32,
    },
}

/// The body length, if it fits the cap.
fn checked_len(len: usize) -> Result<u32, FrameError> {
    match u32::try_from(len) {
        Ok(n) if n <= MAX_FRAME => Ok(n),
        _ => Err(FrameError::TooLarge {
            len: u32::try_from(len).unwrap_or(u32::MAX),
        }),
    }
}

/// Serialize `value` and write it as one frame, then flush.
///
/// # Errors
///
/// `TooLarge` if the JSON is over the cap (nothing is written), `Json` if it
/// cannot be serialized, `Io` if the write fails.
pub fn write(w: &mut impl Write, value: &impl Serialize) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value)?;
    let len = checked_len(body.len())?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

/// Read one frame and parse its body as `T`.
///
/// # Errors
///
/// `TooLarge` if the length is over the cap (the body is not read), `Io` if
/// the stream ends early, `Json` if the body does not parse as `T`.
pub fn read<T: DeserializeOwned>(r: &mut impl Read) -> Result<T, FrameError> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head)?;
    let len = u32::from_be_bytes(head);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge { len });
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

/// [`write`](fn@write) over a tokio writer.
///
/// # Errors
///
/// As [`write`](fn@write).
#[cfg(feature = "tokio")]
pub async fn write_async(
    w: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &impl Serialize,
) -> Result<(), FrameError> {
    use tokio::io::AsyncWriteExt;
    let body = serde_json::to_vec(value)?;
    let len = checked_len(body.len())?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// [`read`](fn@read) over a tokio reader. Not cancel safe: drop the stream after a
/// cancelled read.
///
/// # Errors
///
/// As [`read`](fn@read).
#[cfg(feature = "tokio")]
pub async fn read_async<T: DeserializeOwned>(
    r: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<T, FrameError> {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 4];
    r.read_exact(&mut head).await?;
    let len = u32::from_be_bytes(head);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge { len });
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}
