//! An identity's authenticated calls to the local origin through agmend.

use std::path::PathBuf;

use agmen_client::{IdentityFile, Page, TokenStore, read_local_credential};
use agmen_local::ErrorCode;
use agmen_proto::{ClientEnvelope, IdentityId, ProjectId, Stream};
use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agmend::{Agmend, AgmendError, LaunchCredential, LocalToken};
use crate::client::blocking;
use crate::cmd::lanes::{ItemList, LaneList, PathCheck};
use crate::joined::Recorded;
use crate::output::{Code, Fail};

/// Credentials stay here and are never sent to a hub or broker.
#[derive(Debug)]
pub(crate) struct LocalLink {
    socket: PathBuf,
    identity: IdentityId,
    project: ProjectId,
    token: LocalToken,
}

impl LocalLink {
    /// Uses a matching launch credential, otherwise the saved local credential.
    pub(crate) async fn load(identity: &IdentityFile, tokens: &TokenStore) -> Result<Self> {
        let launch = if std::env::var_os("AGMEN_IDENTITY").as_deref()
            == Some(std::ffi::OsStr::new(identity.identity_id.as_str()))
        {
            LaunchCredential::from_env()?
        } else {
            None
        };
        let token = match launch {
            Some(launch) => launch.token,
            None => {
                let (tokens, id) = (tokens.clone(), identity.identity_id.clone());
                let credential = blocking(move || Ok(read_local_credential(&tokens, &id)?))
                    .await?
                    .ok_or_else(|| {
                        Fail::new(
                            Code::NotLoggedIn,
                            format!("start agmend: no local token for {}", identity.handle),
                        )
                    })?;
                if credential.origin != "local" || credential.owner_id != identity.owner_id {
                    anyhow::bail!("the local identity credential does not match this worktree");
                }
                credential.local_token.parse()?
            }
        };
        Ok(Self {
            socket: agmen_client::agmend_socket()?,
            identity: identity.identity_id.clone(),
            project: identity.project.clone(),
            token,
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut daemon = Agmend::connect_checked(&self.socket).await?;
        daemon
            .call_as(&self.identity, &self.token, method, params)
            .await
            .map_err(local_error)
    }

    pub(crate) async fn post(&self, envelope: &ClientEnvelope) -> Result<Recorded> {
        #[derive(Deserialize)]
        struct Posted {
            stream: Stream,
            seq: u64,
        }
        let posted: Posted =
            serde_json::from_value(self.call("post", json!({"envelope": envelope})).await?)
                .context("agmend's post answer is invalid")?;
        Ok(Recorded {
            id: envelope.id.clone(),
            stream: posted.stream,
            seq: posted.seq,
        })
    }

    pub(crate) async fn page(&self, stream: &Stream, after: u64, limit: u32) -> Result<Page> {
        serde_json::from_value(
            self.call(
                "page",
                json!({"stream": stream, "after": after, "limit": limit}),
            )
            .await?,
        )
        .context("agmend's page answer is invalid")
    }

    pub(crate) async fn state(&self, key: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct State {
            value: Value,
        }
        let answer: State = serde_json::from_value(
            self.call("state", json!({"project": self.project, "key": key}))
                .await?,
        )
        .context("agmend's state answer is invalid")?;
        Ok(answer.value)
    }

    pub(crate) async fn lanes(&self) -> Result<LaneList> {
        serde_json::from_value(self.call("lanes", json!({})).await?)
            .context("agmend's lanes answer is invalid")
    }

    pub(crate) async fn items(&self) -> Result<ItemList> {
        serde_json::from_value(self.call("items", json!({})).await?)
            .context("agmend's items answer is invalid")
    }

    pub(crate) async fn check_path(&self, paths: Vec<String>) -> Result<Vec<PathCheck>> {
        #[derive(Deserialize)]
        struct Checks {
            checks: Vec<PathCheck>,
        }
        let answer: Checks =
            serde_json::from_value(self.call("check_path", json!({"paths": paths})).await?)
                .context("agmend's check_path answer is invalid")?;
        Ok(answer.checks)
    }
}

#[cfg(test)]
impl LocalLink {
    /// A link to a fake agmend at `socket`, for tests whose token is not real.
    pub(crate) fn at(socket: PathBuf, identity: &IdentityFile, token: LocalToken) -> Self {
        Self {
            socket,
            identity: identity.identity_id.clone(),
            project: identity.project.clone(),
            token,
        }
    }
}

/// Keep daemon messages intact; only a final protocol rejection exits 4.
fn local_error(error: AgmendError) -> anyhow::Error {
    match error {
        AgmendError::Refused {
            code: ErrorCode::Hub,
            message,
        } if message.starts_with("hub rejected identity message: ") => {
            Fail::rejected(message.trim_start_matches("hub rejected identity message: ")).into()
        }
        AgmendError::Refused { message, .. } => anyhow::Error::msg(message),
        other => other.into(),
    }
}
