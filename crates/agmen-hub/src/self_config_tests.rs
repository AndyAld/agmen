use std::fs;
use std::time::Duration;

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};

use super::*;
use crate::broker_tokens::BrokerTokens;
use crate::config::HubSecrets;

#[test]
fn generated_secrets_are_created_once() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    let first = load(&secrets, "agmen-hub").unwrap();
    let key_path = dir.path().join("jwt/hub-1.pem");
    let api_path = dir.path().join("broker/api-keys.txt");
    let key = fs::read(&key_path).unwrap();
    let api = fs::read(&api_path).unwrap();
    let key_modified = fs::metadata(&key_path).unwrap().modified().unwrap();
    let api_modified = fs::metadata(&api_path).unwrap().modified().unwrap();
    let second = load(&secrets, "agmen-hub").unwrap();
    assert_eq!(first.signing.kid(), "hub-1");
    assert_eq!(second.signing.kid(), "hub-1");
    let first_tokens = BrokerTokens::new(first.signing, vec![]).unwrap();
    let second_tokens = BrokerTokens::new(second.signing, vec![]).unwrap();
    assert_eq!(first_tokens.jwks(), second_tokens.jwks());
    assert_eq!(first.emqx_api_secret, second.emqx_api_secret);
    assert!(fs::read(&key_path).unwrap() == key);
    assert!(fs::read(&api_path).unwrap() == api);
    assert_eq!(
        fs::metadata(&key_path).unwrap().modified().unwrap(),
        key_modified
    );
    assert_eq!(
        fs::metadata(&api_path).unwrap().modified().unwrap(),
        api_modified
    );
}

#[cfg(unix)]
#[test]
fn generated_files_have_their_modes() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    load(&secrets, "agmen-hub").unwrap();
    for (path, mode) in [
        ("jwt", 0o700),
        ("jwt/hub-1.pem", 0o600),
        ("broker", 0o750),
        ("broker/api-keys.txt", 0o640),
    ] {
        assert_eq!(
            fs::metadata(dir.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode,
            "{path}"
        );
    }
}

/// Runs `run` and returns its result with everything it logged.
#[cfg(unix)]
fn logged<T>(run: impl FnOnce() -> T) -> (T, String) {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let buffer = Buffer(Arc::default());
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, run);
    let log = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    (result, log)
}

#[cfg(unix)]
#[test]
fn an_existing_key_file_with_another_mode_keeps_it_and_is_reported() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    let first = load(&secrets, "agmen-hub").unwrap();
    let api_path = dir.path().join("broker/api-keys.txt");
    // 0644 is what a hub from before the tightening left behind; 0600 is
    // what a restrictive umask would have made.
    for mode in [0o644, 0o600] {
        fs::set_permissions(&api_path, fs::Permissions::from_mode(mode)).unwrap();
        let (second, log) = logged(|| load(&secrets, "agmen-hub").unwrap());
        assert_eq!(second.emqx_api_secret, first.emqx_api_secret);
        assert_eq!(
            fs::metadata(&api_path).unwrap().permissions().mode() & 0o777,
            mode
        );
        assert!(log.contains("WARN"), "{log}");
        assert!(log.contains(&format!("mode={mode:04o}")), "{log}");
        assert!(log.contains("chmod 0640"), "{log}");
        assert!(log.contains(&api_path.display().to_string()), "{log}");
        assert!(!log.contains(second.emqx_api_secret.expose()), "{log}");
    }
}

#[cfg(unix)]
#[test]
fn a_new_key_file_is_not_reported() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    let (_, first) = logged(|| load(&secrets, "agmen-hub").unwrap());
    let (_, second) = logged(|| load(&secrets, "agmen-hub").unwrap());
    assert!(!first.contains("api-keys.txt"), "{first}");
    assert!(!second.contains("api-keys.txt"), "{second}");
}

#[test]
fn api_keys_file_format() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    let loaded = load(&secrets, "agmen-hub").unwrap();
    let content = fs::read_to_string(dir.path().join("broker/api-keys.txt")).unwrap();
    assert!(
        regex::Regex::new(r"\Aagmen-hub:[0-9a-f]{64}:administrator\n\z")
            .unwrap()
            .is_match(&content)
    );
    assert!(
        content
            == format!(
                "agmen-hub:{}:administrator\n",
                loaded.emqx_api_secret.expose()
            )
    );
    let pem = fs::read_to_string(dir.path().join("jwt/hub-1.pem")).unwrap();
    let mut lines = pem.lines();
    assert!(lines.next() == Some("-----BEGIN PRIVATE KEY-----"));
    let mut body = lines.collect::<Vec<_>>();
    assert!(body.pop() == Some("-----END PRIVATE KEY-----"));
    assert!(body.iter().all(|line| !line.is_empty() && line.len() <= 64));
    assert!(body[..body.len() - 1].iter().all(|line| line.len() == 64));
}

#[test]
fn generated_key_signs_tokens_the_jwks_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(
        &HubSecrets::Generated {
            data_dir: dir.path().to_owned(),
        },
        "agmen-hub",
    )
    .unwrap();
    let tokens = BrokerTokens::new(loaded.signing, vec![]).unwrap();
    let token = tokens.mint_hub(Duration::from_secs(60)).unwrap();
    let jwks = tokens.jwks();
    let key = DecodingKey::from_jwk(&jwks.keys[0]).unwrap();
    let claims = decode::<serde_json::Value>(&token, &key, &Validation::new(Algorithm::ES256))
        .unwrap()
        .claims;
    assert_eq!(claims["sub"], "agmen-hub");
    assert_eq!(
        jsonwebtoken::decode_header(&token).unwrap().kid.as_deref(),
        Some("hub-1")
    );
}

#[cfg(unix)]
#[test]
fn unwritable_data_dir_names_the_path() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let dir = tempfile::tempdir().unwrap();
    if fs::metadata(dir.path()).unwrap().uid() == 0 {
        eprintln!(
            "skipping unwritable_data_dir_names_the_path: root bypasses directory permissions"
        );
        return;
    }
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = load(
        &HubSecrets::Generated {
            data_dir: dir.path().to_owned(),
        },
        "agmen-hub",
    );
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.expect_err("read-only directory must fail");
    assert!(matches!(error, SelfConfigError::Io { .. }));
    assert!(
        error
            .to_string()
            .contains(&dir.path().display().to_string())
    );
}

#[test]
fn malformed_api_keys_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    load(&secrets, "agmen-hub").unwrap();
    let path = dir.path().join("broker/api-keys.txt");
    let valid = fs::read_to_string(&path).unwrap();
    for content in [
        String::new(),
        "agmen-hub:short".to_owned(),
        valid.trim_end().to_owned(),
        valid.replace("administrator", "viewer"),
        valid.replacen("agmen-hub", "different-key", 1),
        format!("{valid}{valid}"),
    ] {
        fs::write(&path, &content).unwrap();
        let error = load(&secrets, "agmen-hub").expect_err("malformed file must fail");
        assert!(matches!(&error, SelfConfigError::Malformed { path: bad } if bad == &path));
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(fs::read_to_string(&path).unwrap() == content);
    }
}

#[test]
fn supplied_secrets_keep_the_key_id_and_api_secret() {
    let dir = tempfile::tempdir().unwrap();
    let generated = load(
        &HubSecrets::Generated {
            data_dir: dir.path().to_owned(),
        },
        "custom-key",
    )
    .unwrap();
    let path = dir.path().join("jwt/rotated.pem");
    fs::rename(dir.path().join("jwt/hub-1.pem"), &path).unwrap();
    let given = load(
        &HubSecrets::Given {
            jwt_signing_key: path,
            emqx_api_secret: generated.emqx_api_secret.clone(),
        },
        "unused",
    )
    .unwrap();
    assert_eq!(given.signing.kid(), "rotated");
    assert_eq!(given.emqx_api_secret, generated.emqx_api_secret);
    let tokens = BrokerTokens::new(given.signing, vec![generated.signing]).unwrap();
    assert_eq!(tokens.jwks().keys.len(), 2);
    assert!(
        load(
            &HubSecrets::Generated {
                data_dir: dir.path().to_owned()
            },
            "different-key"
        )
        .is_err()
    );
}

#[test]
fn oversized_secret_files_are_refused_without_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    load(&secrets, "agmen-hub").unwrap();
    let key_path = dir.path().join("jwt/hub-1.pem");
    let api_path = dir.path().join("broker/api-keys.txt");
    let key = fs::read(&key_path).unwrap();
    let oversized = vec![b'x'; 64 * 1024 + 1];
    fs::write(&key_path, &oversized).unwrap();
    assert!(load(&secrets, "agmen-hub").is_err());
    assert!(fs::read(&key_path).unwrap() == oversized);
    fs::write(&key_path, key).unwrap();
    fs::write(&api_path, &oversized).unwrap();
    assert!(load(&secrets, "agmen-hub").is_err());
    assert!(fs::read(&api_path).unwrap() == oversized);
}

#[test]
fn extra_signing_keys_are_bounded_and_keep_their_key_ids() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    load(&secrets, "agmen-hub").unwrap();
    let path = dir.path().join("jwt/extra.pem");
    fs::rename(dir.path().join("jwt/hub-1.pem"), &path).unwrap();
    assert_eq!(load_signing_key(&path).unwrap().kid(), "extra");
    fs::write(&path, vec![b'x'; 64 * 1024 + 1]).unwrap();
    assert!(matches!(
        load_signing_key(&path),
        Err(SelfConfigError::Malformed { .. })
    ));
}

#[test]
fn racing_first_starts_share_one_key_or_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = HubSecrets::Generated {
        data_dir: dir.path().to_owned(),
    };
    let results = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| scope.spawn(|| load(&secrets, "agmen-hub")))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    // A start that loses the lock fails with the lock path; none may see a
    // different key or a partial file.
    let mut winners = Vec::new();
    for result in results {
        match result {
            Ok(loaded) => winners.push(loaded),
            Err(SelfConfigError::Io { path, .. }) => {
                assert_eq!(path, dir.path().join(".self-config.lock"));
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert!(!winners.is_empty());
    let settled = load(&secrets, "agmen-hub").unwrap();
    let expected = BrokerTokens::new(settled.signing, vec![]).unwrap().jwks();
    for winner in winners {
        assert_eq!(winner.emqx_api_secret, settled.emqx_api_secret);
        assert_eq!(
            BrokerTokens::new(winner.signing, vec![]).unwrap().jwks(),
            expected
        );
    }
    let leftovers = fs::read_dir(dir.path().join("jwt"))
        .unwrap()
        .chain(fs::read_dir(dir.path().join("broker")).unwrap())
        .count();
    assert_eq!(leftovers, 2, "only the two secret files remain");
}
