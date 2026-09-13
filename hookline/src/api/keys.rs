//! API credentials, managed through the API itself.
//!
//! The first key cannot be, which is the point: it is minted from the command
//! line by whoever can reach the machine. Everything after that is a request
//! from a key that already has the admin scope.

use super::{text, Api};
use crate::auth::{self, Identity, Scope};
use crate::error::Result;
use crate::models::ApiKey;
use crate::store;
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct New {
    pub name: String,
    /// `admin`, `publish` or `read`. Defaults to `publish`, which is the one
    /// an application server should be given.
    pub scope: Option<String>,
}

#[derive(Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub key: ApiKey,
    /// Shown once. There is no route that will show it again, because what is
    /// stored is a hash.
    pub token: String,
}

pub async fn create(
    State(api): State<Api>,
    identity: Identity,
    Json(body): Json<New>,
) -> Result<Json<Created>> {
    identity.require_write()?;
    let name = text("name", &body.name, 200)?;
    let scope = match body.scope.as_deref() {
        Some(s) => Scope::parse(s)?,
        None => Scope::Publish,
    };
    let minted = auth::mint(&api.db, &name, scope).await?;
    Ok(Json(Created {
        key: minted.key,
        token: minted.token,
    }))
}

pub async fn list(State(api): State<Api>, identity: Identity) -> Result<Json<Vec<ApiKey>>> {
    identity.require_read()?;
    Ok(Json(api.db.call(|conn| store::keys::list(conn)).await?))
}

pub async fn revoke(
    State(api): State<Api>,
    identity: Identity,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    identity.require_write()?;
    let now = crate::now_millis();
    // Revoking the key you are using is allowed. It is occasionally what
    // someone means to do, and refusing it would mean a compromised admin key
    // cannot revoke itself.
    let revoked_self = id == identity.key_id;
    api.db
        .call(move |conn| store::keys::revoke(conn, &id, now))
        .await?;
    Ok(Json(
        serde_json::json!({ "revoked": true, "was_this_key": revoked_self }),
    ))
}
