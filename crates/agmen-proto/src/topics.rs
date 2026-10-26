//! MQTT topic names (protocol section 2).

use crate::error::ProtoError;
use crate::ids::{IdentityId, ProjectId, TenantId};

const ROOT: &str = "agmen/v1";

/// Builds the topic names for one project, under
/// `agmen/v1/{tenant_id}/{project_id}/`.
///
/// ```
/// # fn main() -> Result<(), agmen_proto::ProtoError> {
/// use agmen_proto::{Topics, parse_up};
/// let topics = Topics::new("tenant01".parse()?, "project1".parse()?);
/// let up = topics.up(&"idn7k2q9".parse()?);
/// assert_eq!(up, "agmen/v1/tenant01/project1/up/idn7k2q9");
/// let (_tenant, _project, sender) = parse_up(&up)?;
/// assert_eq!(sender.as_str(), "idn7k2q9");
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Topics {
    tenant: TenantId,
    project: ProjectId,
}

impl Topics {
    /// Topics for one project.
    #[must_use]
    pub const fn new(tenant: TenantId, project: ProjectId) -> Self {
        Self { tenant, project }
    }

    /// The tenant.
    #[must_use]
    pub const fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// The project.
    #[must_use]
    pub const fn project(&self) -> &ProjectId {
        &self.project
    }

    fn topic(&self, rest: &str) -> String {
        format!("{ROOT}/{}/{}/{rest}", self.tenant, self.project)
    }

    /// `up/{id}`: where an identity publishes; only the hub reads it.
    #[must_use]
    pub fn up(&self, id: &IdentityId) -> String {
        self.topic(&format!("up/{id}"))
    }

    /// `feed`: the project-wide stream.
    #[must_use]
    pub fn feed(&self) -> String {
        self.topic("feed")
    }

    /// `inbox/{id}`: one identity's stream.
    #[must_use]
    pub fn inbox(&self, id: &IdentityId) -> String {
        self.topic(&format!("inbox/{id}"))
    }

    /// `ack/{id}`: delivery replies for one identity's publishes.
    #[must_use]
    pub fn ack(&self, id: &IdentityId) -> String {
        self.topic(&format!("ack/{id}"))
    }

    /// `state/head`: the retained head with the stream and state versions.
    #[must_use]
    pub fn state_head(&self) -> String {
        self.state("head")
    }

    /// `state/{key}`: a retained state head such as `roster`.
    ///
    /// The key comes from hub code, never from a client, and must be one topic
    /// level: no `/`, `+` or `#`.
    #[must_use]
    pub fn state(&self, key: &str) -> String {
        debug_assert!(
            !key.is_empty() && !key.contains(['/', '+', '#']),
            "state key {key:?} is not one topic level"
        );
        self.topic(&format!("state/{key}"))
    }

    /// `presence/{id}`: one identity's retained online status.
    #[must_use]
    pub fn presence(&self, id: &IdentityId) -> String {
        self.topic(&format!("presence/{id}"))
    }
}

/// Splits `agmen/v1/{tenant}/{project}/up/{identity}` into its ids. The hub
/// takes the authenticated sender from this, never from the payload.
///
/// # Errors
///
/// [`ProtoError::NotUpTopic`] for any other topic shape, and
/// [`ProtoError::Malformed`] when a level is not a valid id.
pub fn parse_up(topic: &str) -> Result<(TenantId, ProjectId, IdentityId), ProtoError> {
    let rest = topic
        .strip_prefix(ROOT)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or(ProtoError::NotUpTopic)?;
    let mut levels = rest.split('/');
    let (Some(tenant), Some(project), Some("up"), Some(id), None) = (
        levels.next(),
        levels.next(),
        levels.next(),
        levels.next(),
        levels.next(),
    ) else {
        return Err(ProtoError::NotUpTopic);
    };
    Ok((tenant.parse()?, project.parse()?, id.parse()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_build_and_parse() {
        let tenant: TenantId = "tenant01".parse().unwrap();
        let project: ProjectId = "project1".parse().unwrap();
        let id: IdentityId = "abcdefgh".parse().unwrap();
        let topics = Topics::new(tenant.clone(), project.clone());
        let up = topics.up(&id);
        assert_eq!(up, "agmen/v1/tenant01/project1/up/abcdefgh");
        assert_eq!(parse_up(&up).unwrap(), (tenant, project, id.clone()));
        assert_eq!(topics.feed(), "agmen/v1/tenant01/project1/feed");
        assert_eq!(
            topics.inbox(&id),
            "agmen/v1/tenant01/project1/inbox/abcdefgh"
        );
        assert_eq!(topics.ack(&id), "agmen/v1/tenant01/project1/ack/abcdefgh");
        assert_eq!(topics.state_head(), "agmen/v1/tenant01/project1/state/head");
        assert_eq!(
            topics.state("roster"),
            "agmen/v1/tenant01/project1/state/roster"
        );
        assert_eq!(
            topics.presence(&id),
            "agmen/v1/tenant01/project1/presence/abcdefgh"
        );
        for bad in [
            "agmen/v1/tenant01/project1/feed",
            "agmen/v2/tenant01/project1/up/abcdefgh",
            "agmen/v1/tenant01/project1/up/abcdefgh/extra",
            "agmen/v1/tenant01/project1/up/+",
            "agmen/v1/t/p/up/abcdefgh",
            "agmen/v1/tenant01/project1/inbox/abcdefgh",
        ] {
            assert!(parse_up(bad).is_err(), "{bad:?} parsed");
        }
    }
}
