//! Bounded reads shared by the hub API and token grants.

/// The largest non-page hub response body retained, on success or failure.
pub const MAX_HUB_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// The budget for one stamped envelope in a replay page.
const STAMPED_ENVELOPE_BUDGET_BYTES: usize = 80 * 1024;
/// Space for the page's JSON framing and metadata.
const PAGE_JSON_FRAMING_BYTES: usize = 1024 * 1024;
/// The largest replay page body retained, on success or failure.
pub const MAX_PAGE_RESPONSE_BYTES: usize =
    crate::hub::PAGE_LIMIT as usize * STAMPED_ENVELOPE_BUDGET_BYTES + PAGE_JSON_FRAMING_BYTES;

/// Why a bounded hub response could not be read.
#[derive(Debug, thiserror::Error)]
pub enum BodyReadError {
    /// Reading the response failed.
    #[error(transparent)]
    Http(reqwest::Error),
    /// The declared or streamed body exceeded the cap.
    #[error("the hub's response body exceeds the {limit} byte limit")]
    TooLarge {
        /// The maximum response body size.
        limit: usize,
    },
}

/// Reads a hub response, refusing declared or streamed bodies over the cap.
///
/// ```no_run
/// # async fn example(response: reqwest::Response) -> Result<(), agmen_client::BodyReadError> {
/// let bytes = agmen_client::read_hub_body(response).await?;
/// assert!(bytes.len() <= agmen_client::MAX_HUB_RESPONSE_BYTES);
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// Returns [`BodyReadError::TooLarge`] before reading an oversized declared
/// body, or when streamed bytes exceed the cap. Transport failures return
/// [`BodyReadError::Http`].
pub async fn read_hub_body(response: reqwest::Response) -> Result<Vec<u8>, BodyReadError> {
    read_hub_body_with_limit(response, MAX_HUB_RESPONSE_BYTES).await
}

/// Reads a hub response with the endpoint's declared and streamed byte limit.
///
/// # Errors
///
/// Returns [`BodyReadError::TooLarge`] if the body exceeds `limit`, or
/// [`BodyReadError::Http`] on transport failure.
pub async fn read_hub_body_with_limit(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, BodyReadError> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(BodyReadError::TooLarge { limit });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| BodyReadError::Http(error.with_url(response.url().clone())))?
    {
        if chunk.len() > limit - body.len() {
            return Err(BodyReadError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
