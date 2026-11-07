//! Project routing between this machine and the person's hub.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_client::{HubClient, LOCAL_ORIGIN, Login, LoginError, is_local};
use agmen_local::console::RosterEntry;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{IdentityKind, ProjectId};
use url::Url;

use crate::{
    hub::{Hub, HubError},
    identities::{Database, store_work},
    local_origin::{self, LocalOrigin},
    state::Store,
};

/// An unreadable record permits hub routing only after this bounded lookup.
const HUB_PROJECT_LOOKUP_WAIT: Duration = Duration::from_secs(3);
/// Ruling out a local identity in agmend.db before that lookup is bounded the same way.
const LOCAL_IDENTITY_LOOKUP_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Local,
    Hub,
}

#[derive(Debug)]
pub(crate) struct Origins {
    local: Arc<LocalOrigin>,
    hub: Arc<Hub>,
    database: Arc<Database>,
}

impl Origins {
    pub(crate) fn new(
        local: Arc<LocalOrigin>,
        hub: Arc<Hub>,
        store: Arc<Mutex<Store>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            local,
            hub,
            database: Arc::new(Database::new(store)),
        })
    }

    pub(crate) fn local(&self) -> &Arc<LocalOrigin> {
        &self.local
    }
    pub(crate) fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    pub(crate) async fn of(&self, project: &ProjectId) -> Result<Origin, ApiError> {
        match self.local.topics(project).await {
            Ok(_) => Ok(Origin::Local),
            Err(error) if error.code == ErrorCode::NotFound => Ok(Origin::Hub),
            // A busy or stopped origin says nothing about its record, so no hub is asked.
            Err(error) if local_origin::is_unavailable(&error) => Err(error),
            Err(error) => {
                // agmend.db stays readable when local.db is not: a project it holds a
                // local identity for never goes to a hub.
                if self.has_local_identity(project).await {
                    return Err(unreadable(&error));
                }
                // A person-only local project with an unreadable local.db is placed by the signed-in hub's roster; agmend trusts its own hub there.
                // A successful roster read confirms the person's membership without
                // sending a local envelope to a hub that cannot know its project.
                let confirmed = tokio::time::timeout(HUB_PROJECT_LOOKUP_WAIT, async {
                    let (login, token) = self.hub.credentials().access().await.map_err(|_| ())?;
                    let hub = HubClient::new(
                        login.hub_url,
                        project.clone(),
                        login.human_id.clone(),
                        token,
                    )
                    .map_err(|_| ())?;
                    let rows: Vec<RosterEntry> =
                        serde_json::from_value(hub.roster().await.map_err(|_| ())?)
                            .map_err(|_| ())?;
                    rows.iter()
                        .any(|row| {
                            row.identity_id == login.human_id && row.kind == IdentityKind::Human
                        })
                        .then_some(())
                        .ok_or(())
                })
                .await;
                if matches!(confirmed, Ok(Ok(_))) {
                    Ok(Origin::Hub)
                } else {
                    Err(unreadable(&error))
                }
            }
        }
    }

    /// Whether agmend.db registers a local identity in `project`. An agmend.db that
    /// cannot answer counts as yes, so a failed read never opens the hub route.
    async fn has_local_identity(&self, project: &ProjectId) -> bool {
        let wanted = project.clone();
        let outcome = tokio::time::timeout(
            LOCAL_IDENTITY_LOOKUP_WAIT,
            store_work(self.database.clone(), move |store| {
                store.has_identity(LOCAL_ORIGIN, &wanted)
            }),
        )
        .await;
        match outcome {
            Ok(Ok(found)) => found,
            outcome => {
                tracing::warn!(%project, ?outcome, "agmend.db could not rule out a local identity");
                true
            }
        }
    }

    pub(crate) async fn person(&self, origin: Origin) -> Result<Login, ApiError> {
        match origin {
            Origin::Local => self.local.login().await?.ok_or_else(|| ApiError {
                code: ErrorCode::NotFound,
                message: "no local person on this machine".into(),
            }),
            Origin::Hub => self
                .hub
                .credentials()
                .login()
                .await
                .map_err(|error| match error {
                    LoginError::NotSignedIn => ApiError {
                        code: ErrorCode::NotFound,
                        message: "project not found on this machine; not signed in to a hub (run `agmen login`)".into(),
                    },
                    error => HubError::from(error).into(),
                }),
        }
    }

    #[expect(
        dead_code,
        reason = "consumed by origin-aware identity authorization in Task 9"
    )]
    pub(crate) async fn person_for(&self, hub_url: &Url) -> Result<Login, ApiError> {
        self.person(if is_local(hub_url) {
            Origin::Local
        } else {
            Origin::Hub
        })
        .await
    }
}

/// The answer for a record that cannot be read: it names local.db and never a hub.
fn unreadable(error: &ApiError) -> ApiError {
    ApiError {
        code: ErrorCode::Version,
        message: format!("local.db cannot be read: {}", error.message),
    }
}
