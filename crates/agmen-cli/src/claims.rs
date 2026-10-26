//! The claims this identity holds, as the CLI saw them granted, with the
//! commit each one started from: DONE's `base_sha` (`git diff
//! <base_sha_at_claim>...<commit>`). They live in `claims.db` in the
//! identity's state folder, beside agmen-client's `agmen.db`, whose schema
//! stays agmen-client's own. `agmen mcp` (Task 13) uses the same file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_proto::{GitSha, ItemKey, Timestamp};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};

/// The file's name in the state folder.
const CLAIMS_DB: &str = "claims.db";
/// How long a connection waits for another process's write lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// The most claims `items` returns; a HEARTBEAT names each.
///
/// shortcut: a HEARTBEAT renews at most 256 claims, and `items` warns
/// when more are recorded; send several HEARTBEATs if one identity ever
/// holds that many.
const MAX_ITEMS: usize = 256;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS claims (
    item TEXT PRIMARY KEY,
    base_sha TEXT NOT NULL,
    claimed_at_ms INTEGER NOT NULL
) STRICT;
";

/// The claims file of one identity. Blocking: use it off the async
/// runtime's worker threads.
#[derive(Debug)]
pub(crate) struct Claims {
    conn: Connection,
    /// The file, for error messages.
    path: PathBuf,
}

impl Claims {
    /// Opens (creating it if needed) the claims file in `state_dir`.
    pub(crate) fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        let path = state_dir.join(CLAIMS_DB);
        let conn =
            Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .with_context(|| format!("setting the lock timeout of {}", path.display()))?;
        conn.execute_batch(SCHEMA)
            .with_context(|| format!("creating the table in {}", path.display()))?;
        Ok(Self { conn, path })
    }

    /// Records that `item` is claimed with `base` checked out, and returns
    /// whether it added the row. A claim already recorded (a renewed grant)
    /// keeps its first base.
    pub(crate) fn record(&self, item: &ItemKey, base: &GitSha) -> Result<bool> {
        let added = self
            .conn
            .execute(
                "INSERT INTO claims (item, base_sha, claimed_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT (item) DO NOTHING",
                params![
                    item.as_str(),
                    base.as_str(),
                    Timestamp::now().as_millisecond()
                ],
            )
            .with_context(|| format!("recording the claim of {item} in {}", self.path.display()))?;
        Ok(added == 1)
    }

    /// The commit `item`'s claim started from, if it is recorded.
    pub(crate) fn base(&self, item: &ItemKey) -> Result<Option<GitSha>> {
        let base: Option<String> = self
            .conn
            .query_row(
                "SELECT base_sha FROM claims WHERE item = ?1",
                params![item.as_str()],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("reading the claim of {item} in {}", self.path.display()))?;
        base.map(|sha| {
            sha.parse()
                .with_context(|| format!("{} holds an invalid commit id", self.path.display()))
        })
        .transpose()
    }

    /// The recorded claims, oldest first, at most [`MAX_ITEMS`] of them; a
    /// warning on stderr says when more are recorded.
    pub(crate) fn items(&self) -> Result<Vec<ItemKey>> {
        let reading = || format!("reading the claims in {}", self.path.display());
        let mut stmt = self
            .conn
            .prepare("SELECT item FROM claims ORDER BY claimed_at_ms, item LIMIT ?1")
            .with_context(reading)?;
        // One row past the cap tells whether the cap cut any off.
        let limit = i64::try_from(MAX_ITEMS + 1).unwrap_or(i64::MAX);
        let mut items = stmt
            .query_map(params![limit], |row| row.get::<_, String>(0))
            .with_context(reading)?
            .map(|item| {
                item.with_context(reading)?
                    .parse()
                    .with_context(|| format!("{} holds an invalid item key", self.path.display()))
            })
            .collect::<Result<Vec<ItemKey>>>()?;
        if items.len() > MAX_ITEMS {
            items.truncate(MAX_ITEMS);
            tracing::warn!(
                path = %self.path.display(),
                "more than {MAX_ITEMS} claims are recorded; this HEARTBEAT renews the oldest {MAX_ITEMS}"
            );
        }
        Ok(items)
    }

    /// Forgets `item`'s claim.
    pub(crate) fn remove(&self, item: &ItemKey) -> Result<()> {
        self.conn
            .execute("DELETE FROM claims WHERE item = ?1", params![item.as_str()])
            .with_context(|| {
                format!("removing the claim of {item} from {}", self.path.display())
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_keep_the_first_base() {
        let dir = tempfile::tempdir().unwrap();
        let claims = Claims::open(dir.path()).unwrap();
        let (b1, b2): (ItemKey, ItemKey) = ("B-1".parse().unwrap(), "B-2".parse().unwrap());
        let (first, second): (GitSha, GitSha) =
            ("aaaaaaa".parse().unwrap(), "bbbbbbb".parse().unwrap());
        assert!(claims.record(&b1, &first).unwrap(), "added");
        assert!(!claims.record(&b1, &second).unwrap(), "kept the first");
        assert!(claims.record(&b2, &second).unwrap());
        assert_eq!(claims.base(&b1).unwrap(), Some(first));
        assert_eq!(claims.items().unwrap(), [b1.clone(), b2.clone()]);
        claims.remove(&b1).unwrap();
        assert_eq!(claims.base(&b1).unwrap(), None);
        // A second open of the same file sees the rows.
        let again = Claims::open(dir.path()).unwrap();
        assert_eq!(again.items().unwrap(), [b2]);
    }

    #[test]
    fn items_stop_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let claims = Claims::open(dir.path()).unwrap();
        let base: GitSha = "aaaaaaa".parse().unwrap();
        for n in 1..=MAX_ITEMS + 1 {
            let item: ItemKey = format!("B-{n}").parse().unwrap();
            claims.record(&item, &base).unwrap();
        }
        assert_eq!(claims.items().unwrap().len(), MAX_ITEMS);
    }

    #[test]
    fn errors_name_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let claims = Claims::open(dir.path()).unwrap();
        claims.conn.execute_batch("DROP TABLE claims").unwrap();
        let err = claims.items().unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("reading the claims in"), "{text}");
        assert!(text.contains("claims.db"), "{text}");
    }
}
