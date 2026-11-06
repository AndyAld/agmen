//! The reserved local origin and the keys used alongside hub origins.

use url::Url;

use crate::Broker;

/// The key for this machine's local origin.
pub const LOCAL_ORIGIN: &str = "local";
/// The identity-file placeholder for a local project with no hub.
pub const LOCAL_HUB_URL: &str = "https://local.agmen.invalid/";
/// The identity-file placeholder for a local project with no broker.
pub const LOCAL_BROKER_URL: &str = "mqtt://local.agmen.invalid:1883";

/// The local identity-file hub URL. No connection may be made to it.
///
/// ```
/// assert_eq!(agmen_client::local_hub_url().as_str(), agmen_client::LOCAL_HUB_URL);
/// ```
#[must_use]
pub fn local_hub_url() -> Url {
    // A malformed built-in constant is a programming error, never user input.
    Url::parse(LOCAL_HUB_URL).unwrap_or_else(|_| std::process::abort())
}

/// The local identity-file broker placeholder. It has no broker behind it.
///
/// ```
/// assert_eq!(agmen_client::local_broker().to_string(), agmen_client::LOCAL_BROKER_URL);
/// ```
#[must_use]
pub fn local_broker() -> Broker {
    // The constant is checked by the origin tests and has no fallible caller input.
    LOCAL_BROKER_URL
        .parse()
        .unwrap_or_else(|_| std::process::abort())
}

/// Whether the URL's scheme, host and port name the local origin.
/// Paths, queries and fragments do not affect its origin.
///
/// ```
/// # fn main() -> Result<(), url::ParseError> {
/// assert!(agmen_client::is_local(&"https://local.agmen.invalid:443/path".parse()?));
/// assert!(!agmen_client::is_local(&"https://hub.example/".parse()?));
/// # Ok(()) }
/// ```
#[must_use]
pub fn is_local(hub_url: &Url) -> bool {
    hub_url.origin() == local_hub_url().origin()
}

/// The storage key for a local or hub origin.
///
/// ```
/// # fn main() -> Result<(), url::ParseError> {
/// assert_eq!(agmen_client::origin_key(&agmen_client::local_hub_url()), "local");
/// assert_eq!(agmen_client::origin_key(&"https://hub.example/x".parse()?), "https://hub.example");
/// # Ok(()) }
/// ```
#[must_use]
pub fn origin_key(hub_url: &Url) -> String {
    if is_local(hub_url) {
        LOCAL_ORIGIN.to_owned()
    } else {
        hub_url.origin().ascii_serialization()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use agmen_proto::{IdentityId, Stream, Topics};

    use crate::{
        ClientError, ClientKind, HubClient, LOCAL_BROKER_URL, LOCAL_HUB_URL, LOCAL_ORIGIN,
        LOCAL_TOKEN_NAMESPACE, LocalCredential, Session, SessionConfig, TokenKey, TokenSource,
        TokenStore, is_local, local_broker, local_hub_url, origin_key, read_local_credential,
    };

    const LOCAL_WAIT: Duration = Duration::from_millis(100);

    #[test]
    fn local_origin_key_is_local() {
        assert_eq!(LOCAL_ORIGIN, "local");
        assert_eq!(LOCAL_HUB_URL, "https://local.agmen.invalid/");
        assert_eq!(LOCAL_BROKER_URL, "mqtt://local.agmen.invalid:1883");
        assert_eq!(local_hub_url().as_str(), LOCAL_HUB_URL);
        assert_eq!(local_broker().to_string(), LOCAL_BROKER_URL);
        assert_eq!(origin_key(&local_hub_url()), "local");
        assert_eq!(
            origin_key(&"https://hub.example/x".parse().unwrap()),
            "https://hub.example"
        );
        assert_eq!(
            origin_key(&"https://hub.example:8443/x".parse().unwrap()),
            "https://hub.example:8443"
        );
    }

    #[test]
    fn local_origin_matches_only_scheme_host_and_port() {
        for url in [
            "https://local.agmen.invalid/",
            "https://LOCAL.agmen.invalid:443/path?query=yes#fragment",
        ] {
            let url = url.parse().unwrap();
            assert!(is_local(&url));
            assert_eq!(origin_key(&url), "local");
        }
        for url in [
            "http://local.agmen.invalid/",
            "https://local.agmen.invalid:8443/",
            "http://local.agmen.invalid:443/",
            "https://local.agmen.invalid.evil.example/",
            "https://evil.local.agmen.invalid/",
            "https://evil-local.agmen.invalid/",
            "https://local.agmen.invalid@evil.example/",
            "https://local.agmen.invalid:443@evil.example:8443/",
            "https://evil.example/?u=https://local.agmen.invalid/",
            "https://hub.example/",
            "file:///local.agmen.invalid",
        ] {
            let url = url.parse().unwrap();
            assert!(!is_local(&url));
            assert_eq!(origin_key(&url), url.origin().ascii_serialization());
        }
    }

    fn local_hub() -> HubClient {
        HubClient::new(
            local_hub_url(),
            "project1".parse().unwrap(),
            "idn7k2q9".parse().unwrap(),
            "unused-test-token",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn hub_client_refuses_the_local_origin_without_a_lookup() {
        let hub = local_hub();
        let result = tokio::time::timeout(LOCAL_WAIT, hub.page(&Stream::Feed, 0))
            .await
            .expect("local replay must fail before a network lookup");
        assert!(matches!(result, Err(ClientError::LocalOrigin)));
        let result = tokio::time::timeout(LOCAL_WAIT, hub.roster())
            .await
            .expect("local roster must fail before a network lookup");
        assert!(matches!(result, Err(ClientError::LocalOrigin)));
        let result = tokio::time::timeout(LOCAL_WAIT, hub.broker_token())
            .await
            .expect("local broker tokens must fail before a network lookup");
        assert!(matches!(result, Err(ClientError::LocalOrigin)));
        assert_eq!(
            ClientError::LocalOrigin.to_string(),
            "this identity is on this machine's local origin; it has no hub"
        );
    }

    struct CountingTokens(Arc<AtomicUsize>);

    impl TokenSource for CountingTokens {
        async fn token(&self) -> Result<String, ClientError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ClientError::Token("unexpected token request".into()))
        }
    }

    #[tokio::test]
    async fn session_refuses_the_local_origin() {
        let dir = tokio::task::spawn_blocking(|| tempfile::tempdir().unwrap())
            .await
            .unwrap();
        let state = dir.path().join("state");
        let calls = Arc::new(AtomicUsize::new(0));
        let topics = Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap());
        let config = SessionConfig::new(
            ClientKind::Watch,
            topics,
            local_hub(),
            local_broker(),
            &state,
        )
        .with_token_source(CountingTokens(calls.clone()));
        let result = tokio::time::timeout(LOCAL_WAIT, Session::connect(config))
            .await
            .expect("local sessions must fail before opening state or asking for a token");
        assert!(matches!(result, Err(ClientError::LocalOrigin)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tokio::task::spawn_blocking(move || {
            assert!(!state.exists());
            drop(dir);
        })
        .await
        .unwrap();
    }

    #[test]
    fn local_credential_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = TokenStore::Dir(dir.path().join("tokens"));
        let id: IdentityId = "idn7k2q9".parse().unwrap();
        assert!(read_local_credential(&tokens, &id).unwrap().is_none());
        assert_eq!(LOCAL_TOKEN_NAMESPACE, "https://agmend.invalid");
        let key = TokenKey::new(&LOCAL_TOKEN_NAMESPACE.parse().unwrap(), &id).unwrap();
        assert_eq!(key.as_str(), "https://agmend.invalid idn7k2q9");
        let credential = LocalCredential {
            origin: "local".into(),
            owner_id: "andyandy".parse().unwrap(),
            local_token: "local-test-token".into(),
        };
        tokens
            .set(&key, &serde_json::to_string(&credential).unwrap())
            .unwrap();
        let loaded = read_local_credential(&tokens, &id).unwrap().unwrap();
        assert_eq!(loaded.origin, credential.origin);
        assert_eq!(loaded.owner_id, credential.owner_id);
        assert_eq!(loaded.local_token, credential.local_token);
        assert!(
            read_local_credential(&tokens, &"otherid1".parse().unwrap())
                .unwrap()
                .is_none()
        );
        // Existing hub-backed credentials keep their own origin in this record.
        let hub_credential = LocalCredential {
            origin: "https://hub.example".into(),
            ..credential
        };
        tokens
            .set(&key, &serde_json::to_string(&hub_credential).unwrap())
            .unwrap();
        let loaded = read_local_credential(&tokens, &id).unwrap().unwrap();
        assert_eq!(loaded.origin, "https://hub.example");
    }

    #[test]
    fn local_credential_rejects_invalid_records_without_exposing_values() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = TokenStore::Dir(dir.path().join("tokens"));
        let id = "idn7k2q9".parse().unwrap();
        let key = TokenKey::new(&LOCAL_TOKEN_NAMESPACE.parse().unwrap(), &id).unwrap();
        for raw in [
            "not json",
            r#"{"origin":"local","owner_id":"andyandy"}"#,
            r#"{"origin":"local","owner_id":"!","local_token":"private-test-value"}"#,
            r#"{"origin":"local","owner_id":"andyandy","local_token":"test","private-test-value":true}"#,
        ] {
            tokens.set(&key, raw).unwrap();
            let error = match read_local_credential(&tokens, &id) {
                Err(error) => error,
                Ok(_) => panic!("invalid local credential was accepted"),
            };
            assert!(!error.to_string().contains("private-test-value"));
            assert!(!format!("{error:?}").contains("private-test-value"));
        }
    }

    #[test]
    fn local_credential_reports_store_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, "not a directory").unwrap();
        let tokens = TokenStore::Dir(path);
        let result = read_local_credential(&tokens, &"idn7k2q9".parse().unwrap());
        assert!(
            result.is_err(),
            "store failure must not look like a missing credential"
        );
    }
}
