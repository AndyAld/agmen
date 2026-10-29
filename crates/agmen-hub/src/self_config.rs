//! Loads supplied secrets or keeps generated secrets in the hub's data directory.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::broker_tokens::{SigningKey, TokenError};
use crate::config::{HubSecrets, Secret};
use crate::db;

const MAX_FILE_BYTES: u64 = 64 * 1024;
/// The broker API key file's mode: the hub's group (which EMQX joins) reads
/// it and nobody else does.
const BROKER_KEY_MODE: u32 = 0o640;

/// Why the hub's supplied or generated secrets could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SelfConfigError {
    /// Reading, creating or publishing a file failed.
    #[error("accessing hub secrets at {path}")]
    Io {
        /// The file or directory that could not be accessed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A secret file is too large or has an invalid format.
    #[error("malformed hub secrets at {path}")]
    Malformed {
        /// The invalid file.
        path: PathBuf,
    },
    /// Generating a signing key or random secret failed.
    #[error("generating the hub secrets failed")]
    KeyGen,
    /// A signing key could not be parsed.
    #[error("loading the signing key failed: {0}")]
    Key(#[from] TokenError),
}

#[derive(Debug)]
pub(crate) struct Loaded {
    pub(crate) signing: SigningKey,
    pub(crate) emqx_api_secret: Secret,
}

fn io(path: &Path, source: impl Into<std::io::Error>) -> SelfConfigError {
    SelfConfigError::Io {
        path: path.to_owned(),
        source: source.into(),
    }
}

fn malformed(path: &Path) -> SelfConfigError {
    SelfConfigError::Malformed {
        path: path.to_owned(),
    }
}

pub(crate) fn load(secrets: &HubSecrets, api_key: &str) -> Result<Loaded, SelfConfigError> {
    match secrets {
        HubSecrets::Given {
            jwt_signing_key,
            emqx_api_secret,
        } => Ok(Loaded {
            signing: load_signing_key(jwt_signing_key)?,
            emqx_api_secret: emqx_api_secret.clone(),
        }),
        HubSecrets::Generated { data_dir } => generated(data_dir, api_key),
    }
}

pub(crate) fn load_signing_key(path: &Path) -> Result<SigningKey, SelfConfigError> {
    let kid = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| TokenError::NoKeyId(path.display().to_string()))?;
    let pem = read_existing(path)?.ok_or_else(|| {
        io(
            path,
            std::io::Error::new(std::io::ErrorKind::NotFound, "signing key file is missing"),
        )
    })?;
    Ok(SigningKey::from_pem(kid, &pem)?)
}

fn generated(data_dir: &Path, api_key: &str) -> Result<Loaded, SelfConfigError> {
    let api_path = data_dir.join("broker/api-keys.txt");
    if api_key.is_empty()
        || api_key
            .bytes()
            .any(|byte| byte == b':' || byte.is_ascii_control())
        || api_key.len() as u64 + 80 > MAX_FILE_BYTES
    {
        return Err(malformed(&api_path));
    }
    fs::create_dir_all(data_dir).map_err(|source| io(data_dir, source))?;
    // Serialize cooperating hub starts so rename cannot replace another
    // start's secrets. Fail immediately if another hub holds the lock.
    let lock_path = data_dir.join(".self-config.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let lock = options
        .open(&lock_path)
        .map_err(|source| io(&lock_path, source))?;
    lock.try_lock().map_err(|source| io(&lock_path, source))?;

    let jwt_dir = data_dir.join("jwt");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(&jwt_dir)
        .map_err(|source| io(&jwt_dir, source))?;
    #[cfg(unix)]
    fs::set_permissions(&jwt_dir, fs::Permissions::from_mode(0o700))
        .map_err(|source| io(&jwt_dir, source))?;
    let pem = read_or_create(&jwt_dir.join("hub-1.pem"), 0o600, generate_key)?;
    let signing = SigningKey::from_pem("hub-1", &pem)?;

    let broker_dir = data_dir.join("broker");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o750);
    builder
        .create(&broker_dir)
        .map_err(|source| io(&broker_dir, source))?;
    // Compose gives EMQX the hub's group so it can read this shared volume.
    // The modes are requests: the process umask can remove bits from them.
    // shortcut: an existing file keeps its mode and only draws a warning, so a
    // host that an earlier hub created at 0644 needs a manual chmod; add a
    // migration if more than one such host exists.
    let api = read_or_create(&api_path, BROKER_KEY_MODE, || {
        let secret = db::new_secret().map_err(|_| SelfConfigError::KeyGen)?;
        Ok(format!("{api_key}:{secret}:administrator\n").into_bytes())
    })?;
    #[cfg(unix)]
    warn_if_key_mode_differs(&api_path)?;
    let emqx_api_secret = parse_api_secret(&api_path, &api, api_key)?;
    Ok(Loaded {
        signing,
        emqx_api_secret,
    })
}

/// Logs a warning when the key file is not [`BROKER_KEY_MODE`]. The hub never
/// changes the mode of a file that exists, so an earlier hub's 0644 stays,
/// and a restrictive umask can leave a new file narrower than requested.
#[cfg(unix)]
fn warn_if_key_mode_differs(path: &Path) -> Result<(), SelfConfigError> {
    let mode = fs::metadata(path)
        .map_err(|source| io(path, source))?
        .permissions()
        .mode()
        & 0o777;
    if mode != BROKER_KEY_MODE {
        tracing::warn!(
            path = %path.display(),
            mode = %format_args!("{mode:04o}"),
            "the broker API key file is not mode {BROKER_KEY_MODE:04o}: a wider mode lets other users read it, a narrower one stops EMQX reading it through its group; run chmod {BROKER_KEY_MODE:04o} on it as the volume's owner"
        );
    }
    Ok(())
}

fn read_existing(path: &Path) -> Result<Option<Vec<u8>>, SelfConfigError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io(path, source)),
    };
    let metadata = file.metadata().map_err(|source| io(path, source))?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(malformed(path));
    }
    let mut contents = Vec::new();
    (&file)
        .take(MAX_FILE_BYTES)
        .read_to_end(&mut contents)
        .map_err(|source| io(path, source))?;
    if file.metadata().map_err(|source| io(path, source))?.len() > MAX_FILE_BYTES {
        return Err(malformed(path));
    }
    Ok(Some(contents))
}

fn read_or_create(
    path: &Path,
    mode: u32,
    generate: impl FnOnce() -> Result<Vec<u8>, SelfConfigError>,
) -> Result<Vec<u8>, SelfConfigError> {
    if let Some(contents) = read_existing(path)? {
        return Ok(contents);
    }
    let contents = generate()?;
    let name = db::new_id().map_err(|_| SelfConfigError::KeyGen)?;
    let temporary = path.with_extension(format!("{name}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode);
    let mut file = options
        .open(&temporary)
        .map_err(|source| io(&temporary, source))?;
    let result = (|| {
        file.write_all(&contents)
            .map_err(|source| io(&temporary, source))?;
        #[cfg(not(unix))]
        let _ = mode;
        file.sync_all().map_err(|source| io(&temporary, source))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|source| io(path, source))
    })();
    if result.is_err() {
        fs::remove_file(&temporary).map_err(|source| io(&temporary, source))?;
    }
    result?;
    Ok(contents)
}

fn generate_key() -> Result<Vec<u8>, SelfConfigError> {
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .map_err(|_| SelfConfigError::KeyGen)?;
    let encoded = STANDARD.encode(pkcs8.as_ref());
    let mut pem = b"-----BEGIN PRIVATE KEY-----\n".to_vec();
    for line in encoded.as_bytes().chunks(64) {
        pem.extend_from_slice(line);
        pem.push(b'\n');
    }
    pem.extend_from_slice(b"-----END PRIVATE KEY-----\n");
    Ok(pem)
}

fn parse_api_secret(path: &Path, api: &[u8], api_key: &str) -> Result<Secret, SelfConfigError> {
    let api = std::str::from_utf8(api).map_err(|_| malformed(path))?;
    let mut fields = api.split(':');
    let (Some(key), Some(secret), Some(role), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(malformed(path));
    };
    if key != api_key
        || secret.len() != 64
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || role != "administrator\n"
    {
        return Err(malformed(path));
    }
    Ok(Secret::new(secret))
}

#[cfg(test)]
#[path = "self_config_tests.rs"]
mod tests;
