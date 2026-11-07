//! Project routing between this machine and the person's hub.

use std::sync::Arc;

use agmen_client::{Login, LoginError, is_local};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::ProjectId;
use url::Url;

use crate::{
    hub::{Hub, HubError},
    local_origin::LocalOrigin,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Local,
    Hub,
}

#[derive(Debug)]
pub(crate) struct Origins {
    local: Arc<LocalOrigin>,
    hub: Arc<Hub>,
}

impl Origins {
    pub(crate) fn new(local: Arc<LocalOrigin>, hub: Arc<Hub>) -> Arc<Self> {
        Arc::new(Self { local, hub })
    }

    pub(crate) fn local(&self) -> &Arc<LocalOrigin> {
        &self.local
    }
    pub(crate) fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    pub(crate) async fn of(&self, project: &ProjectId) -> Origin {
        if self.local.has_project(project).await {
            Origin::Local
        } else {
            Origin::Hub
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
                        message: "project not found on this machine; not signed in to a hub".into(),
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
