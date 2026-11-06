//! Identity credentials for calls to agmend, kept in the existing token store.

use agmen_proto::IdentityId;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{ClientError, TokenKey, TokenStore};

/// The token-store namespace for credentials issued by agmend.
pub const LOCAL_TOKEN_NAMESPACE: &str = "https://agmend.invalid";

/// An agmend credential for an identity on a local or hub origin.
/// Deliberately has no `Debug` implementation because it holds a token.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[expect(
    missing_debug_implementations,
    reason = "credentials must not appear in diagnostics"
)]
pub struct LocalCredential {
    /// The origin whose identity agmend authenticates.
    pub origin: String,
    /// The person who owns the identity.
    pub owner_id: IdentityId,
    /// The token presented to agmend for this identity.
    pub local_token: String,
}

/// Reads an identity's agmend credential, or `None` if none is stored.
/// This reads a file or the keychain; call it off async runtime worker threads.
///
/// ```no_run
/// # fn main() -> Result<(), agmen_client::ClientError> {
/// use agmen_client::{TokenStore, read_local_credential};
/// let id = "idn7k2q9".parse().map_err(|_| agmen_client::ClientError::LocalCredential)?;
/// if let Some(credential) = read_local_credential(&TokenStore::from_env(), &id)? {
///     println!("credential for {}", credential.origin);
/// }
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`ClientError::LocalCredentialStore`] if the token store cannot be read,
/// or [`ClientError::LocalCredential`] if the stored record is invalid.
pub fn read_local_credential(
    tokens: &TokenStore,
    id: &IdentityId,
) -> Result<Option<LocalCredential>, ClientError> {
    let namespace = Url::parse(LOCAL_TOKEN_NAMESPACE).map_err(|_| ClientError::LocalCredential)?;
    let key = TokenKey::new(&namespace, id).map_err(ClientError::LocalCredentialStore)?;
    let Some(raw) = tokens
        .get(&key)
        .map_err(ClientError::LocalCredentialStore)?
    else {
        return Ok(None);
    };
    // Serde errors may quote credential values or unknown field names.
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| ClientError::LocalCredential)
}
