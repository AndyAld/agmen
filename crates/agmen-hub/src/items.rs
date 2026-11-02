//! The bounded HTTP item list, including security-title filtering.

use agmen_core::ItemList;
use axum::Json;
use axum::extract::{Path, State};

use crate::auth::Caller;
use crate::http::{ApiError, Hub};
use crate::lanes::{member_of, store_error};

/// `GET /v1/projects/{p}/items` (any member): the oldest
/// [`agmen_core::MAX_LISTED`] items, with who holds each.
pub(crate) async fn list(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
) -> Result<Json<ItemList>, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    hub.core()
        .items(&project)
        .await
        .map(Json)
        .map_err(store_error)
}
