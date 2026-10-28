//! The file-backed MQTT session record (ADR-0003). rumqttc-next resumes a
//! session the broker kept only when it has its own record of that
//! session, so each persistent client id keeps one file next to the
//! identity's SQLite file.
//!
//! A record that is missing or unreadable loads as none. If the broker
//! still holds the session, rumqttc then refuses the CONNACK with
//! `SessionStateMismatch`, and the session connects once with a clean start
//! and fills the gap by replay (`crate::session`).

use std::fs;
use std::future::Future;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use rumqttc_next::{PersistedSession, SessionStore, SessionStoreError, SessionStoreKey};

/// The first line of every record; a file without it is not one.
const MAGIC: &[u8] = b"agmen-mqtt-session 1\n";

type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

/// One client id's session record in one file.
///
/// rumqttc saves after most packets and may drop a save it started when
/// the poll that started it is cancelled, so writes are numbered and
/// applied in order: a save that finishes after a later one is skipped.
#[derive(Debug, Clone)]
pub(crate) struct FileSessionStore {
    path: PathBuf,
    order: Arc<Mutex<Order>>,
}

/// The numbers of the last write handed out and the last one applied.
#[derive(Debug, Default)]
struct Order {
    issued: u64,
    applied: u64,
}

impl FileSessionStore {
    /// A store whose record is the file at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            order: Arc::default(),
        }
    }

    /// The next write's number.
    fn ticket(&self) -> u64 {
        let mut order = self.order.lock().unwrap_or_else(PoisonError::into_inner);
        order.issued += 1;
        order.issued
    }

    /// Runs `write` (a save or a clear) on a blocking thread, unless a
    /// later write was applied first.
    fn write_in_order(
        &self,
        write: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
    ) -> StoreFuture<'static, ()> {
        let (store, ticket) = (self.clone(), self.ticket());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut order = store.order.lock().unwrap_or_else(PoisonError::into_inner);
                if ticket < order.applied {
                    return Ok(());
                }
                write(&store.path)?;
                order.applied = ticket;
                Ok(())
            })
            .await
            .map_err(|err| Box::new(err) as SessionStoreError)?
            .map_err(|err: io::Error| Box::new(err) as SessionStoreError)
        })
    }
}

impl SessionStore for FileSessionStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        let (store, scope) = (self.clone(), key.scope().to_owned());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let _order = store.order.lock().unwrap_or_else(PoisonError::into_inner);
                Ok(read_record(&store.path, &scope))
            })
            .await
            .map_err(|err| Box::new(err) as SessionStoreError)?
        })
    }

    fn save<'a>(
        &'a self,
        key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> StoreFuture<'a, ()> {
        let bytes = match session.encode() {
            Ok(bytes) => bytes,
            Err(err) => return Box::pin(async move { Err(Box::new(err) as SessionStoreError) }),
        };
        let mut record = MAGIC.to_vec();
        record.extend_from_slice(key.scope().as_bytes());
        record.push(b'\n');
        record.extend_from_slice(&bytes);
        self.write_in_order(move |path| write_atomically(path, &record))
    }

    fn clear<'a>(&'a self, _key: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        self.write_in_order(|path| match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        })
    }
}

/// The record at `path` for broker `scope`, or `None` when the file is
/// missing, unreadable, from another broker, or not a record.
fn read_record(path: &Path, scope: &str) -> Option<PersistedSession> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot read the MQTT session record; starting a new session");
            return None;
        }
    };
    let rest = bytes.strip_prefix(MAGIC)?;
    let newline = rest.iter().position(|&b| b == b'\n')?;
    let (stored_scope, encoded) = (&rest[..newline], &rest[newline + 1..]);
    if stored_scope != scope.as_bytes() {
        // A record made for another broker says nothing about this one.
        return None;
    }
    match PersistedSession::decode(encoded) {
        Ok(session) => Some(session),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "the MQTT session record is corrupt; starting a new session");
            None
        }
    }
}

/// Writes `bytes` to `path` through a temporary file in the same folder
/// and renames it into place, so a crash leaves the old record or the new
/// one, never part of one.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        // Persists the rename itself. Not every platform can open a folder
        // for this (Windows cannot); the record is still whole without it.
        if let Ok(dir) = fs::File::open(dir) {
            dir.sync_all()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(client_id: &str) -> PersistedSession {
        PersistedSession {
            format_version: 1,
            client_id: client_id.to_owned(),
            clean_start: false,
            session_expiry_interval: Some(86_400),
            outgoing_inflight_upper_limit: None,
            ack_mode: rumqttc_next::PersistedAckMode::Manual,
            last_pkid: 3,
            last_puback: 2,
            replay: Vec::new(),
            incoming_qos2: Vec::new(),
        }
    }

    #[tokio::test]
    async fn saves_loads_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("a-watch.mqtt"));
        let key = SessionStoreKey::new("mqtt://broker:1883", "idn7k2q9-watch");
        assert_eq!(store.load(&key).await.unwrap(), None);
        store.save(&key, &session("idn7k2q9-watch")).await.unwrap();
        assert_eq!(
            store.load(&key).await.unwrap(),
            Some(session("idn7k2q9-watch"))
        );
        let other_broker = SessionStoreKey::new("mqtt://elsewhere:1883", "idn7k2q9-watch");
        assert_eq!(store.load(&other_broker).await.unwrap(), None);
        store.clear(&key).await.unwrap();
        assert_eq!(store.load(&key).await.unwrap(), None);
        store.clear(&key).await.unwrap();
    }

    #[tokio::test]
    async fn a_corrupt_record_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a-watch.mqtt");
        let store = FileSessionStore::new(path.clone());
        let key = SessionStoreKey::new("mqtt://broker:1883", "idn7k2q9-watch");
        fs::write(&path, b"garbage").unwrap();
        assert_eq!(store.load(&key).await.unwrap(), None);
        let mut record = MAGIC.to_vec();
        record.extend_from_slice(b"mqtt://broker:1883\nnot a session");
        fs::write(&path, record).unwrap();
        assert_eq!(store.load(&key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_late_save_does_not_undo_a_later_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path().join("a-watch.mqtt"));
        let key = SessionStoreKey::new("mqtt://broker:1883", "idn7k2q9-watch");
        let (older, newer) = (session("idn7k2q9-watch"), {
            let mut newer = session("idn7k2q9-watch");
            newer.last_pkid = 9;
            newer
        });
        // Numbered when rumqttc asks, as a cancelled poll would leave them,
        // then finished newest first.
        let first = store.save(&key, &older);
        let second = store.save(&key, &newer);
        second.await.unwrap();
        first.await.unwrap();
        assert_eq!(store.load(&key).await.unwrap(), Some(newer));
    }
}
