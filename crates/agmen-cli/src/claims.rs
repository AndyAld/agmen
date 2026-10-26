//! The claims this identity holds, as the CLI saw them granted, with the
//! commit each one started from: DONE's `base_sha` (`git diff
//! <base_sha_at_claim>...<commit>`). They live in `claims.db` in the
//! identity's state folder, beside agmen-client's `agmen.db`, whose schema
//! stays agmen-client's own. `agmen mcp` (Task 13) uses the same file.

use std::path::Path;
use std::time::Duration;

use agmen_proto::{GitSha, ItemKey, Timestamp};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};

/// The file's name in the state folder.
const CLAIMS_DB: &str = "claims.db";
/// How long a connection waits for another process's write lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// The most claims `items` returns; a HEARTBEAT names each.
const MAX_ITEMS: i64 = 256;

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
}

impl Claims {
    /// Opens (creating it if needed) the claims file in `state_dir`.
    pub(crate) fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        let path = state_dir.join(CLAIMS_DB);
        let conn =
            Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(SCHEMA)
            .with_context(|| format!("creating the table in {}", path.display()))?;
        Ok(Self { conn })
    }

    /// Records that `item` was granted with `base` checked out. A claim
    /// already recorded (a renewed grant) keeps its first base.
    pub(crate) fn record(&self, item: &ItemKey, base: &GitSha) -> Result<()> {
        self.conn.execute(
            "INSERT INTO claims (item, base_sha, claimed_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (item) DO NOTHING",
            params![
                item.as_str(),
                base.as_str(),
                Timestamp::now().as_millisecond()
            ],
        )?;
        Ok(())
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
            .optional()?;
        base.map(|sha| sha.parse().context("claims.db holds an invalid commit id"))
            .transpose()
    }

    /// The recorded claims, oldest first.
    pub(crate) fn items(&self) -> Result<Vec<ItemKey>> {
        let mut stmt = self
            .conn
            .prepare("SELECT item FROM claims ORDER BY claimed_at_ms, item LIMIT ?1")?;
        let rows = stmt.query_map(params![MAX_ITEMS], |row| row.get::<_, String>(0))?;
        rows.map(|item| item?.parse().context("claims.db holds an invalid item key"))
            .collect()
    }

    /// Forgets `item`'s claim.
    pub(crate) fn remove(&self, item: &ItemKey) -> Result<()> {
        self.conn
            .execute("DELETE FROM claims WHERE item = ?1", params![item.as_str()])?;
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
        claims.record(&b1, &first).unwrap();
        claims.record(&b1, &second).unwrap();
        claims.record(&b2, &second).unwrap();
        assert_eq!(claims.base(&b1).unwrap(), Some(first));
        assert_eq!(claims.items().unwrap(), [b1.clone(), b2.clone()]);
        claims.remove(&b1).unwrap();
        assert_eq!(claims.base(&b1).unwrap(), None);
        // A second open of the same file sees the rows.
        let again = Claims::open(dir.path()).unwrap();
        assert_eq!(again.items().unwrap(), [b2]);
    }
}
