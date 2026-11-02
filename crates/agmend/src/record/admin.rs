//! Local person, projects and identity administration.

use std::collections::BTreeSet;
use std::io::Read;

use agmen_core::{MemberRole, StoreError};
use agmen_proto::{EditScope, IdentityId, IdentityKind, ProjectId, RoleSpec, TenantId, Topics};
use rusqlite::{Connection, OptionalExtension, params};

use super::{SqliteStore, backend};

pub(crate) const MAX_LOCAL_PROJECTS: usize = 256;
/// The longest person handle: a GitHub login's 39 characters, as on the hub.
const MAX_HANDLE_BYTES: usize = 39;
/// The handle when nothing usable is left of the name offered.
const FALLBACK_HANDLE: &str = "me";
const BAD_PERSON_HANDLE: &str =
    "the person's handle must be 1 to 39 of [a-z0-9-], not starting with -";
const NO_PERSON: &str = "the local person has not been created";

/// Whether `handle` can name the local person. This is the hub's rule for
/// `humans.handle` (`is_human_handle` in `agmen-hub`). Every identity the
/// person mints is named `<handle>/<name>`, so a `/`, a space or a leading
/// `-` here would be copied into the handle of every identity.
pub(crate) fn is_person_handle(handle: &str) -> bool {
    (1..=MAX_HANDLE_BYTES).contains(&handle.len())
        && !handle.starts_with('-')
        && handle
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A person handle from a hub login or an OS user name. ASCII letters are
/// lowercased and every other character is dropped, so a lookalike letter
/// is never folded onto an ASCII one; leading `-` are dropped and the rest
/// is cut to 39. `me` stands in when nothing is left.
pub(crate) fn clean_person_handle(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .skip_while(|c| *c == '-')
        .take(MAX_HANDLE_BYTES)
        .collect();
    if cleaned.is_empty() {
        FALLBACK_HANDLE.to_owned()
    } else {
        cleaned
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalPerson {
    pub(crate) tenant: TenantId,
    pub(crate) id: IdentityId,
    pub(crate) handle: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalMint {
    pub(crate) project: ProjectId,
    pub(crate) name: String,
    pub(crate) kind: IdentityKind,
    pub(crate) spec: Option<RoleSpec>,
    pub(crate) agent: Option<String>,
    pub(crate) delegated_by: Option<IdentityId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Minted {
    pub(crate) id: IdentityId,
    pub(crate) handle: String,
    pub(crate) topics: Topics,
    pub(crate) verbs: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct MintError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl MintError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<StoreError> for MintError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Backend(source) => match source.downcast::<Self>() {
                Ok(error) => return *error,
                Err(source) => tracing::error!(error = %source, "local mint failed"),
            },
            other => tracing::error!(error = %other, "local mint failed"),
        }
        Self::new("server_error", "internal error")
    }
}

impl SqliteStore {
    pub(crate) async fn person(&self) -> Result<Option<LocalPerson>, StoreError> {
        self.read(|conn, _| load_person(conn)).await
    }

    pub(crate) async fn create_person(&self, handle: &str) -> Result<LocalPerson, StoreError> {
        let handle = handle.to_owned();
        self.write(move |conn, now| {
            if let Some(person) = load_person(conn)? {
                return Ok(person);
            }
            if !is_person_handle(&handle) {
                return Err(StoreError::Stored(BAD_PERSON_HANDLE.into()));
            }
            let tenant = new_id()?;
            let id = new_id()?;
            conn.execute(
                "INSERT INTO tenants(id, name, created_at) VALUES (?1, ?1, ?2)",
                params![tenant, now],
            )
            .map_err(backend)?;
            conn.execute(
                "INSERT INTO humans(id, handle, created_at) VALUES (?1, ?2, ?3)",
                params![id, handle, now],
            )
            .map_err(backend)?;
            conn.execute(
                "INSERT INTO local_origin(id, tenant_id, person_id, created_at)
                 VALUES (1, ?1, ?2, ?3)",
                params![tenant, id, now],
            )
            .map_err(backend)?;
            Ok(LocalPerson {
                tenant: tenant.parse().map_err(stored)?,
                id: id.parse().map_err(stored)?,
                handle,
            })
        })
        .await
    }

    pub(crate) async fn create_project(&self, name: &str) -> Result<Topics, StoreError> {
        let name = name.to_owned();
        self.write(move |conn, now| {
            let person = load_person(conn)?.ok_or_else(|| StoreError::Stored(NO_PERSON.into()))?;
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND name=?2)",
                    params![person.tenant.as_str(), name],
                    |row| row.get(0),
                )
                .map_err(backend)?;
            if exists {
                return Err(StoreError::Duplicate);
            }
            let project = new_id()?;
            conn.execute(
                "INSERT INTO projects(id, tenant_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![project, person.tenant.as_str(), name, now],
            )
            .map_err(backend)?;
            conn.execute(
                "INSERT INTO memberships(human_id, project_id, role, created_at)
                 VALUES (?1, ?2, 'admin', ?3)",
                params![person.id.as_str(), project, now],
            )
            .map_err(backend)?;
            Ok(Topics::new(person.tenant, project.parse().map_err(stored)?))
        })
        .await
    }

    pub(crate) async fn projects(&self) -> Result<Vec<(ProjectId, String)>, StoreError> {
        self.read(|conn, _| {
            let mut stmt = conn
                .prepare(
                    "SELECT p.id, p.name FROM projects p
                     JOIN local_origin o ON o.tenant_id=p.tenant_id
                     WHERE o.id=1 ORDER BY p.name, p.id LIMIT ?1",
                )
                .map_err(backend)?;
            let rows = stmt
                .query_map([MAX_LOCAL_PROJECTS as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(backend)?;
            rows.map(|row| {
                let (id, name) = row.map_err(backend)?;
                Ok((id.parse().map_err(stored)?, name))
            })
            .collect()
        })
        .await
    }

    pub(crate) async fn topics(&self, project: &ProjectId) -> Result<Option<Topics>, StoreError> {
        let project = project.clone();
        self.read(move |conn, _| load_topics(conn, &project)).await
    }

    pub(crate) async fn mint(&self, request: LocalMint) -> Result<Minted, MintError> {
        self.write(move |conn, now| {
            // Preserve validation refusals through write so they roll back too.
            mint(conn, now, request).map_err(|err| StoreError::Backend(Box::new(err)))
        })
        .await
        .map_err(MintError::from)
    }

    pub(crate) async fn revoke(
        &self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<bool, StoreError> {
        let project = project.clone();
        let id = id.clone();
        self.write(move |conn, now| {
            conn.execute(
                "UPDATE identities SET revoked_at=?1
                 WHERE project_id=?2 AND id=?3 AND revoked_at IS NULL
                   AND tenant_id=(SELECT tenant_id FROM local_origin WHERE id=1)",
                params![now, project.as_str(), id.as_str()],
            )
            .map(|changed| changed == 1)
            .map_err(backend)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn add_member(
        &self,
        topics: &Topics,
        role: MemberRole,
    ) -> Result<IdentityId, StoreError> {
        let topics = topics.clone();
        self.write(move |conn, now| {
            if load_topics(conn, topics.project())?.as_ref() != Some(&topics) {
                return Err(StoreError::Stored("the project is not local".into()));
            }
            let id = new_id()?;
            conn.execute(
                "INSERT INTO humans(id, handle, created_at) VALUES (?1, ?1, ?2)",
                params![id, now],
            )
            .map_err(backend)?;
            conn.execute(
                "INSERT INTO memberships(human_id, project_id, role, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, topics.project().as_str(), role.as_str(), now],
            )
            .map_err(backend)?;
            id.parse().map_err(stored)
        })
        .await
    }
}

fn stored(err: impl std::fmt::Display) -> StoreError {
    StoreError::Stored(err.to_string())
}

fn load_person(conn: &Connection) -> Result<Option<LocalPerson>, StoreError> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT o.tenant_id, h.id, h.handle FROM local_origin o
             JOIN humans h ON h.id=o.person_id WHERE o.id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(backend)?;
    row.map(|(tenant, id, handle)| {
        Ok(LocalPerson {
            tenant: tenant.parse().map_err(stored)?,
            id: id.parse().map_err(stored)?,
            handle,
        })
    })
    .transpose()
}

fn load_topics(conn: &Connection, project: &ProjectId) -> Result<Option<Topics>, StoreError> {
    let tenant: Option<String> = conn
        .query_row(
            "SELECT p.tenant_id FROM projects p
             JOIN local_origin o ON o.tenant_id=p.tenant_id
             WHERE o.id=1 AND p.id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    tenant
        .map(|tenant| {
            Ok(Topics::new(
                tenant.parse().map_err(stored)?,
                project.clone(),
            ))
        })
        .transpose()
}

fn new_id() -> Result<String, StoreError> {
    const ID_LEN: usize = 16;
    // The hub's 32-character alphabet gives each character equal probability.
    const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = [0_u8; ID_LEN];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|err| StoreError::Backend(Box::new(err)))?;
    Ok(bytes
        .into_iter()
        .map(|byte| char::from(ALPHABET[usize::from(byte & 31)]))
        .collect())
}

fn mint(conn: &Connection, now: i64, request: LocalMint) -> Result<Minted, MintError> {
    let not_member = || MintError::new("not_a_member", "you are not a member of that project");
    let person = load_person(conn)?.ok_or_else(not_member)?;
    let topics = load_topics(conn, &request.project)?.ok_or_else(not_member)?;
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM memberships WHERE human_id=?1 AND project_id=?2",
            params![person.id.as_str(), request.project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    let role = parse_role(&role.ok_or_else(not_member)?)?;
    if !agmen_core::is_child_name(&request.name) {
        return Err(MintError::new(
            "bad_handle",
            format!(
                "the handle must be {}/<name>, where name is 1 to 32 of [a-z0-9-] \
                 starting with a letter or digit",
                person.handle,
            ),
        ));
    }
    if !matches!(
        request.kind,
        IdentityKind::Agent | IdentityKind::Coordinator | IdentityKind::Listener
    ) {
        // The hub's MintKind deserializer excludes these before its handler.
        return Err(MintError::new(
            "bad_role",
            "kind must be agent, coordinator or listener",
        ));
    }
    let coordinator = request.kind == IdentityKind::Coordinator
        || (request.kind == IdentityKind::Listener
            && request
                .spec
                .as_ref()
                .is_some_and(|spec| spec.role.as_str() == "coordinator"));
    if coordinator && role < MemberRole::Coordinator {
        return Err(MintError::new(
            "coordinator_role_required",
            "minting a coordinator needs the coordinator role in the project",
        ));
    }
    let permissions = permissions(request.kind, request.spec)?;
    if request
        .agent
        .as_ref()
        .is_some_and(|agent| agent.len() > agmen_proto::MAX_HELLO_FIELD_BYTES)
    {
        return Err(MintError::new("bad_role", "agent is too long"));
    }
    if let Some(id) = &request.delegated_by {
        if request.kind != IdentityKind::Agent {
            return Err(bad_delegator());
        }
        check_delegator(conn, &request.project, &person.id, id)?;
    }
    let handle = format!("{}/{}", person.handle, request.name);
    check_available(conn, &request.project, &person.id, request.kind, &handle)?;
    let id = new_id()?;
    let verbs = permissions.as_ref().map(|spec| spec.verbs.clone());
    let verbs_json = verbs
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(stored)?;
    let edits_json = permissions
        .as_ref()
        .map(|spec| serde_json::to_string(&spec.edits))
        .transpose()
        .map_err(stored)?;
    conn.execute(
        "INSERT INTO identities(id, tenant_id, project_id, kind, handle,
             owner_human_id, created_at, role, verbs, edits, role_sha256,
             agent, delegated_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            topics.tenant().as_str(),
            request.project.as_str(),
            request.kind.as_str(),
            handle,
            person.id.as_str(),
            now,
            permissions.as_ref().map(|spec| spec.role.as_str()),
            verbs_json,
            edits_json,
            permissions.as_ref().map(|spec| spec.role_sha256.as_str()),
            request.agent,
            request.delegated_by.as_ref().map(IdentityId::as_str),
        ],
    )
    .map_err(backend)?;
    Ok(Minted {
        id: id.parse().map_err(stored)?,
        handle,
        topics,
        verbs,
    })
}

/// The hub parses the stored role before it compares it; a name this build
/// does not know is an internal error, never a lower or higher role.
fn parse_role(role: &str) -> Result<MemberRole, StoreError> {
    [
        MemberRole::Member,
        MemberRole::Coordinator,
        MemberRole::Admin,
    ]
    .into_iter()
    .find(|known| known.as_str() == role)
    .ok_or_else(|| StoreError::Stored(format!("unknown membership role {role:?}")))
}

fn permissions(kind: IdentityKind, spec: Option<RoleSpec>) -> Result<Option<RoleSpec>, MintError> {
    let Some(mut spec) = spec else {
        return Ok(None);
    };
    agmen_core::validate_edits(&spec.edits)
        .map_err(|message| MintError::new("bad_edits", message))?;
    if kind == IdentityKind::Listener && spec.edits != EditScope::None {
        return Err(MintError::new(
            "bad_role",
            "a listener's edits must be none",
        ));
    }
    spec.verbs = agmen_core::cap_verbs(kind, &spec.verbs);
    if kind == IdentityKind::Listener && spec.role.as_str() != "coordinator" {
        spec.verbs.remove("ASSIGN");
    }
    Ok(Some(spec))
}

fn check_available(
    conn: &Connection,
    project: &ProjectId,
    owner: &IdentityId,
    kind: IdentityKind,
    handle: &str,
) -> Result<(), MintError> {
    // BEGIN IMMEDIATE excludes competing writers until this mint commits.
    // Checking these keys separately keeps unrelated constraint errors internal.
    if kind == IdentityKind::Listener {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM identities
                 WHERE project_id=?1 AND owner_human_id=?2
                   AND kind='listener' AND revoked_at IS NULL)",
                params![project.as_str(), owner.as_str()],
                |row| row.get(0),
            )
            .map_err(backend)?;
        if exists {
            return Err(MintError::new(
                "listener_exists",
                "this owner already has a live listener in this project",
            ));
        }
    }
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM identities
             WHERE project_id=?1 AND handle=?2 AND revoked_at IS NULL)",
            params![project.as_str(), handle],
            |row| row.get(0),
        )
        .map_err(backend)?;
    if exists {
        return Err(MintError::new(
            "handle_taken",
            format!("a live identity in this project is already {handle}"),
        ));
    }
    Ok(())
}

fn bad_delegator() -> MintError {
    MintError::new(
        "bad_role",
        "delegated_by must name a live listener of this owner and project for an agent worker",
    )
}

/// The write transaction keeps this check and its worker insert atomic.
pub(crate) fn check_delegator(
    conn: &Connection,
    project: &ProjectId,
    owner: &IdentityId,
    id: &IdentityId,
) -> Result<(), MintError> {
    let valid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM identities
             WHERE id=?1 AND owner_human_id=?2 AND project_id=?3
               AND kind='listener' AND revoked_at IS NULL)",
            params![id.as_str(), owner.as_str(), project.as_str()],
            |row| row.get(0),
        )
        .map_err(backend)?;
    if !valid {
        return Err(bad_delegator());
    }
    Ok(())
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
