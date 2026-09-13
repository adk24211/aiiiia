//! Applications: the tenants everything else hangs off.

use super::{text, Api, Paging};
use crate::auth::Identity;
use crate::error::{Error, Result};
use crate::models::{App, Page};
use crate::store;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct New {
    pub name: String,
    /// Your identifier for this tenant. Unique, and usable in place of the id
    /// we generate in every route below.
    pub uid: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

pub async fn create(
    State(api): State<Api>,
    identity: Identity,
    Json(body): Json<New>,
) -> Result<Json<App>> {
    identity.require_write()?;
    let name = text("name", &body.name, 200)?;
    let uid = match &body.uid {
        Some(uid) => Some(text("uid", uid, 200)?),
        None => None,
    };
    let metadata = if body.metadata.is_null() {
        serde_json::json!({})
    } else {
        body.metadata
    };
    let now = crate::now_millis();
    let app = api
        .db
        .call(move |conn| store::apps::create(conn, &name, uid.as_deref(), &metadata, now))
        .await?;
    Ok(Json(app))
}

pub async fn list(
    State(api): State<Api>,
    identity: Identity,
    Query(paging): Query<Paging>,
) -> Result<Json<Page<App>>> {
    identity.require_read()?;
    let limit = paging.limit();
    let cursor = paging.cursor;
    let page = api
        .db
        .call(move |conn| store::apps::list(conn, cursor.as_deref(), limit))
        .await?;
    Ok(Json(page))
}

pub async fn get(
    State(api): State<Api>,
    identity: Identity,
    Path(reference): Path<String>,
) -> Result<Json<App>> {
    identity.require_read()?;
    let app = api
        .db
        .call(move |conn| store::apps::get(conn, &reference))
        .await?;
    Ok(Json(app))
}

#[derive(Deserialize)]
pub struct Patch {
    pub name: Option<String>,
    pub uid: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

pub async fn update(
    State(api): State<Api>,
    identity: Identity,
    Path(reference): Path<String>,
    Json(body): Json<Patch>,
) -> Result<Json<App>> {
    identity.require_write()?;
    let name = match &body.name {
        Some(name) => Some(text("name", name, 200)?),
        None => None,
    };
    let uid = match &body.uid {
        Some(uid) => Some(text("uid", uid, 200)?),
        None => None,
    };
    let metadata = body.metadata;
    let app = api
        .db
        .call(move |conn| {
            store::apps::update(
                conn,
                &reference,
                name.as_deref(),
                uid.as_deref(),
                metadata.as_ref(),
            )
        })
        .await?;
    Ok(Json(app))
}

/// Delete an application and everything under it.
///
/// Irreversible, and the only route that can destroy history, so it wants the
/// application's name repeated back: a delete that happens because a script
/// had the wrong id in a variable is the kind of accident worth one extra
/// field to prevent.
#[derive(Deserialize, Default)]
pub struct Confirm {
    pub confirm_name: Option<String>,
}

pub async fn delete(
    State(api): State<Api>,
    identity: Identity,
    Path(reference): Path<String>,
    Query(confirm): Query<Confirm>,
) -> Result<Json<serde_json::Value>> {
    identity.require_write()?;
    let app = api
        .db
        .call({
            let reference = reference.clone();
            move |conn| store::apps::get(conn, &reference)
        })
        .await?;
    match confirm.confirm_name.as_deref() {
        Some(given) if given == app.name => {}
        _ => {
            return Err(Error::invalid(format!(
                "deleting an application destroys its endpoints, messages and history; \
                 repeat its name as confirm_name={:?} to go ahead",
                app.name
            )))
        }
    }
    let id = app.id.clone();
    api.db
        .call(move |conn| store::apps::delete(conn, &id))
        .await?;
    Ok(Json(serde_json::json!({ "deleted": app.id })))
}
