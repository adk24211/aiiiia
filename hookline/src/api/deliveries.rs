//! Deliveries and attempts: the history, and the routes that act on it.
//!
//! Replay is the reason this history is stored at all. A consumer that was
//! down for an hour, or whose handler had a bug, wants the events back — and
//! without a replay route their only options are to ask you to re-emit them
//! from your own database, or to do without.

use super::{app_id, Api, Paging};
use crate::auth::Identity;
use crate::error::Result;
use crate::models::{Attempt, Delivery, DeliveryStatus, Page};
use crate::store;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Default)]
pub struct Filter {
    /// `pending`, `succeeded`, `failed` or `cancelled`.
    pub status: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

pub async fn for_endpoint(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
    Query(filter): Query<Filter>,
) -> Result<Json<Page<Delivery>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let status = match &filter.status {
        Some(s) => Some(DeliveryStatus::parse(s)?),
        None => None,
    };
    let limit = filter.paging.limit();
    let cursor = filter.paging.cursor;
    let page = api
        .db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            store::deliveries::list_for_endpoint(
                conn,
                &app_id,
                &endpoint,
                status,
                cursor.as_deref(),
                limit,
            )
        })
        .await?;
    Ok(Json(page))
}

pub async fn for_message(
    State(api): State<Api>,
    identity: Identity,
    Path((app, message)): Path<(String, String)>,
) -> Result<Json<Vec<Delivery>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| {
            store::messages::get(conn, &app_id, &message)?;
            store::deliveries::for_message(conn, &app_id, &message)
        })
        .await?;
    Ok(Json(found))
}

pub async fn get(
    State(api): State<Api>,
    identity: Identity,
    Path((app, delivery)): Path<(String, String)>,
) -> Result<Json<Delivery>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| store::deliveries::get(conn, &app_id, &delivery))
        .await?;
    Ok(Json(found))
}

pub async fn attempts(
    State(api): State<Api>,
    identity: Identity,
    Path((app, delivery)): Path<(String, String)>,
) -> Result<Json<Vec<Attempt>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| {
            store::deliveries::get(conn, &app_id, &delivery)?;
            store::attempts::for_delivery(conn, &app_id, &delivery)
        })
        .await?;
    Ok(Json(found))
}

#[derive(Deserialize, Default)]
pub struct AttemptFilter {
    /// `success` or `failure`.
    pub status: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

pub async fn attempts_for_endpoint(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
    Query(filter): Query<AttemptFilter>,
) -> Result<Json<Page<Attempt>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let status = filter.status;
    let limit = filter.paging.limit();
    let cursor = filter.paging.cursor;
    let page = api
        .db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            store::attempts::list_for_endpoint(
                conn,
                &app_id,
                &endpoint,
                status.as_deref(),
                cursor.as_deref(),
                limit,
            )
        })
        .await?;
    Ok(Json(page))
}

pub async fn replay(
    State(api): State<Api>,
    identity: Identity,
    Path((app, delivery)): Path<(String, String)>,
) -> Result<Json<Delivery>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let now = crate::now_millis();
    let replayed = api
        .db
        .call(move |conn| {
            let d = store::deliveries::replay(conn, &app_id, &delivery, now)?;
            store::health::close_circuit(conn, &d.endpoint_id)?;
            Ok(d)
        })
        .await?;
    api.wake.poke();
    Ok(Json(replayed))
}

pub async fn cancel(
    State(api): State<Api>,
    identity: Identity,
    Path((app, delivery)): Path<(String, String)>,
) -> Result<Json<Delivery>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let now = crate::now_millis();
    let cancelled = api
        .db
        .call(move |conn| store::deliveries::cancel(conn, &app_id, &delivery, now))
        .await?;
    Ok(Json(cancelled))
}

#[derive(Deserialize, Default)]
pub struct Bulk {
    /// Only replay deliveries created at or after this millisecond timestamp.
    pub since: Option<i64>,
    /// Only replay deliveries created at or before this one.
    pub until: Option<i64>,
    /// Which states to replay. Defaults to the failed ones, since replaying a
    /// success means sending a consumer something they already have.
    pub status: Option<String>,
    /// The most to replay in one call.
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct Replayed {
    pub replayed: usize,
    /// True when the limit was reached and there is more to do: call again
    /// with `since` set past the last one.
    pub more: bool,
}

/// Replay an endpoint's history in bulk.
///
/// Bounded and repeatable rather than a single sweeping call: an endpoint with
/// a month of failures behind it would otherwise queue a hundred thousand
/// deliveries in one transaction, and the consumer that just came back up
/// would receive all of them at once.
pub async fn replay_endpoint(
    State(api): State<Api>,
    identity: Identity,
    Path((app, endpoint)): Path<(String, String)>,
    body: Option<Json<Bulk>>,
) -> Result<Json<Replayed>> {
    identity.require_write()?;
    let app_id = app_id(&api.db, &app).await?;
    let Json(bulk) = body.unwrap_or_default();
    let status = match bulk.status.as_deref() {
        Some(s) => DeliveryStatus::parse(s)?,
        None => DeliveryStatus::Failed,
    };
    let limit = store::clamp_limit(bulk.limit);
    let since = bulk.since.unwrap_or(0);
    let until = bulk.until.unwrap_or(i64::MAX);
    let now = crate::now_millis();

    let (replayed, more) = api
        .db
        .call(move |conn| {
            store::endpoints::get(conn, &app_id, &endpoint)?;
            let tx = crate::db::write_tx(conn)?;
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM deliveries
                     WHERE app_id = ?1 AND endpoint_id = ?2 AND status = ?3
                       AND created_at >= ?4 AND created_at <= ?5
                     ORDER BY id LIMIT ?6",
                )?;
                let rows = stmt
                    .query_map(
                        rusqlite::params![app_id, endpoint, status, since, until, limit as i64 + 1],
                        |r| r.get::<_, String>(0),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let more = ids.len() > limit;
            let mut count = 0;
            for id in ids.into_iter().take(limit) {
                store::deliveries::replay(&tx, &app_id, &id, now)?;
                count += 1;
            }
            store::health::close_circuit(&tx, &endpoint)?;
            tx.commit()?;
            Ok((count, more))
        })
        .await?;

    if replayed > 0 {
        api.wake.poke();
    }
    Ok(Json(Replayed { replayed, more }))
}
