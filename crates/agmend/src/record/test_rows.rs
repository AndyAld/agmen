//! Raw rows shared by the SQLite adapter's unit tests.

use agmen_proto::{IdentityId, Topics};
use rusqlite::{Connection, params};

fn id(conn: &Connection) -> String {
    conn.query_row("SELECT lower(hex(randomblob(10)))", [], |row| row.get(0))
        .unwrap()
}

pub(super) fn project(conn: &Connection) -> Topics {
    let tenant = id(conn);
    let project = id(conn);
    conn.execute(
        "INSERT INTO tenants(id, name, created_at) VALUES (?1, ?1, 0)",
        [&tenant],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO projects(id, tenant_id, name, created_at) VALUES (?1, ?2, ?1, 0)",
        params![project, tenant],
    )
    .unwrap();
    Topics::new(tenant.parse().unwrap(), project.parse().unwrap())
}

pub(super) fn human(conn: &Connection, topics: &Topics, role: &str) -> IdentityId {
    let id = id(conn);
    conn.execute(
        "INSERT INTO humans(id, handle, created_at) VALUES (?1, ?1, 0)",
        [&id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memberships(human_id, project_id, role, created_at) VALUES (?1, ?2, ?3, 0)",
        params![id, topics.project().as_str(), role],
    )
    .unwrap();
    id.parse().unwrap()
}

pub(super) fn identity(
    conn: &Connection,
    topics: &Topics,
    kind: &str,
    owner: &IdentityId,
) -> IdentityId {
    let id = id(conn);
    conn.execute(
        "INSERT INTO identities(id, tenant_id, project_id, kind, handle, owner_human_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?1, ?5, 0)",
        params![id, topics.tenant().as_str(), topics.project().as_str(), kind, owner.as_str()],
    )
    .unwrap();
    id.parse().unwrap()
}
