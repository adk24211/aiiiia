//! Endpoints, and the secrets their requests are signed with.

use super::{app_id, text, Api, Paging};
use crate::auth::Identity;
use crate::error::{Error, Result};
use crate::guard;
use crate::models::{Endpoint, Health, Page, Secret};
use crate::sign;
use crate::store;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct New {
    pub url: String,
    #[serde(default)]
    pub description: String,
    /// The event types to send here. Omit for all of them. A trailing `*`
    /// matches a prefix, so `invoice.*` covers `invoice.paid`.
    pub event_types: Option<Vec<String>>,
    pub rate_limit: Option<u32>,
    /// Supply your own signing secret, if you are migrating from something
    /// else and consumers already have one. Otherwise one is generated.
    pub secret: Option<String>,
}

#[derive(Serialize)]
pub struct CreatedEndpoint {
    #[serde(flatten)]
    pub endpoint: Endpoint,
    /// The signing secret. Returned here and from the secrets routes, which
    /// are the only places it appears.
    pub secret: String,
}

pub async fn create(
    State(api): State<Api>,
    identity: Identity,
    Path(app): Path<String>,
    Json(body): Json<New>,
) -> Result<Json<CreatedEndpoint>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;

    // The URL is judged now, so a typo or a private address is a 400 here
    // rather than a delivery that fails hours later for reasons the person
    // who typed it will not connect to what they typed.
    guard::check_url(&body.url, &api.config.destinations)
        .map_err(|why| Error::invalid(format!("{}", why)))?;

    let description = body.description.trim().to_string();
    if description.len() > 500 {
        return Err(Error::invalid("description is over the 500 byte limit"));
    }
    let event_types = validate_types(body.event_types)?;
    let secret = match &body.secret {
        Some(s) => text("secret", s, 500)?,
        None => sign::new_secret(),
    };
    let url = body.url.clone();
    let rate_limit = body.rate_limit;
    let now = crate::now_millis();

    let (endpoint, secret) = api
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let created = store::endpoints::create(
                &tx,
                &app_id,
                &url,
                &description,
                event_types.as_deref(),
                rate_limit,
                &secret,
                now,
            )?;
            tx.commit()?;
            Ok(created)
        })
        .await?;
    Ok(Json(CreatedEndpoint {
        endpoint,
        secret: secret.secret,
    }))
}

pub async fn list(
    State(api): State<Api>,
    identity: Identity,
    Path(app): Path<String>,
    Query(paging): Query<Paging>,
) -> Result<Json<Page<Endpoint>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let limit = paging.limit();
    let cursor = paging.cursor;
    let page = api
        .db
        .call(move |conn| store::endpoints::list(conn, &app_id, cursor.as_deref(), limit))
        .await?;
    Ok(Json(page))
}

pub async fn get(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
) -> Result<Json<Endpoint>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| store::endpoints::get(conn, &app_id, &endpoint))
        .await?;
    Ok(Json(found))
}

#[derive(Deserialize)]
pub struct Patch {
    pub url: Option<String>,
    pub description: Option<String>,
    /// Present and null clears the filter; absent leaves it alone.
    #[serde(default, deserialize_with = "explicit_null")]
    pub event_types: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub rate_limit: Option<Option<u32>>,
}

/// Tell "the field was null" apart from "the field was absent".
///
/// serde collapses both to `None` in an `Option<T>`; a webhook filter is
/// exactly the field where clearing it and not mentioning it must not mean the
/// same thing.
fn explicit_null<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

pub async fn update(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
    Json(body): Json<Patch>,
) -> Result<Json<Endpoint>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    if let Some(url) = &body.url {
        guard::check_url(url, &api.config.destinations)
            .map_err(|why| Error::invalid(format!("{}", why)))?;
    }
    let event_types = match body.event_types {
        None => None,
        Some(None) => Some(None),
        Some(Some(types)) => Some(validate_types(Some(types))?),
    };
    let url = body.url;
    let description = body.description;
    let rate_limit = body.rate_limit;
    let now = crate::now_millis();
    let updated = api
        .db
        .call(move |conn| {
            store::endpoints::update(
                conn,
                &app_id,
                &endpoint,
                url.as_deref(),
                description.as_deref(),
                event_types.as_ref().map(|t| t.as_deref()),
                rate_limit,
                now,
            )
        })
        .await?;
    Ok(Json(updated))
}

pub async fn delete(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    api.db
        .call(move |conn| store::endpoints::delete(conn, &app_id, &endpoint))
        .await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Deserialize, Default)]
pub struct Reason {
    pub reason: Option<String>,
}

pub async fn disable(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
    body: Option<Json<Reason>>,
) -> Result<Json<Endpoint>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let reason = body
        .and_then(|Json(r)| r.reason)
        .unwrap_or_else(|| "disabled through the API".to_string());
    let now = crate::now_millis();
    let updated = api
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let e = store::endpoints::disable(&tx, &app_id, &endpoint, &reason, now)?;
            tx.commit()?;
            Ok(e)
        })
        .await?;
    Ok(Json(updated))
}

pub async fn enable(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
) -> Result<Json<Endpoint>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let now = crate::now_millis();
    let updated = api
        .db
        .call(move |conn| store::endpoints::enable(conn, &app_id, &endpoint, now))
        .await?;
    api.wake.poke();
    Ok(Json(updated))
}

pub async fn health(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
) -> Result<Json<Health>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            store::health::get(conn, &endpoint)
        })
        .await?;
    Ok(Json(found))
}

pub async fn secrets(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
) -> Result<Json<Vec<Secret>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            store::secrets::list(conn, &endpoint)
        })
        .await?;
    Ok(Json(found))
}

#[derive(Deserialize, Default)]
pub struct Rotation {
    /// Supply the new secret, or let one be generated.
    pub secret: Option<String>,
    /// How long the old secrets keep signing. Defaults to the server's
    /// configured grace period.
    pub grace_secs: Option<u64>,
}

/// Add a secret and give the existing ones a deadline.
///
/// Both sign until the grace period is up, which is what makes a rotation a
/// change the consumer can make on their own schedule instead of a cutover
/// that has to be coordinated.
pub async fn rotate(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
    body: Option<Json<Rotation>>,
) -> Result<Json<Secret>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let Json(body) = body.unwrap_or_default();
    let secret = match &body.secret {
        Some(s) => text("secret", s, 500)?,
        None => sign::new_secret(),
    };
    let grace = body
        .grace_secs
        .map(|s| s as i64 * 1000)
        .unwrap_or(api.config.secret_grace.as_millis() as i64);
    let now = crate::now_millis();
    let created = api
        .db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            let tx = conn.transaction()?;
            let s = store::secrets::rotate(&tx, &endpoint, &secret, grace, now)?;
            tx.commit()?;
            Ok(s)
        })
        .await?;
    Ok(Json(created))
}

pub async fn revoke(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint, secret)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let now = crate::now_millis();
    api.db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            store::secrets::revoke(conn, &endpoint, &secret, now)
        })
        .await?;
    Ok(Json(serde_json::json!({ "revoked": true })))
}

/// Reject a filter list that cannot match anything useful.
fn validate_types(types: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    let Some(types) = types else { return Ok(None) };
    if types.len() > 200 {
        return Err(Error::invalid(
            "an endpoint may filter on at most 200 event types",
        ));
    }
    let mut cleaned = Vec::with_capacity(types.len());
    for t in types {
        let t = t.trim().to_string();
        if t.is_empty() {
            return Err(Error::invalid("an event type must not be empty"));
        }
        if t.len() > 200 {
            return Err(Error::invalid("an event type is over the 200 byte limit"));
        }
        // A star anywhere but the end would read as a wildcard and silently
        // not be one.
        if t[..t.len() - 1].contains('*') {
            return Err(Error::invalid(format!(
                "{:?}: a wildcard is only allowed at the end, as in invoice.*",
                t
            )));
        }
        cleaned.push(t);
    }
    Ok(Some(cleaned))
}
