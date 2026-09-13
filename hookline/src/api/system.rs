//! Health, version and queue statistics.

use super::Api;
use crate::auth::Identity;
use crate::error::Result;
use crate::queue;
use axum::extract::State;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub uptime_ms: i64,
}

/// Unauthenticated, and deliberately shallow: it answers "is this process
/// serving requests", which is the question a load balancer is asking. The
/// question "is the queue keeping up" is a different one with a different
/// answer, and it is in `/v1/stats` behind a credential.
pub async fn health(State(api): State<Api>) -> Json<Health> {
    Json(Health {
        status: "ok",
        uptime_ms: crate::now_millis() - api.started_at,
    })
}

#[derive(Serialize)]
pub struct Version {
    pub name: &'static str,
    pub version: &'static str,
}

pub async fn version() -> Json<Version> {
    Json(Version {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Serialize)]
pub struct Stats {
    pub queue: queue::Depth,
    pub apps: i64,
    pub endpoints: i64,
    pub endpoints_disabled: i64,
    pub circuits_open: i64,
    /// Attempts in the last hour, and how many of them succeeded. The ratio is
    /// the number worth graphing.
    pub attempts_last_hour: i64,
    pub successes_last_hour: i64,
    pub uptime_ms: i64,
}

pub async fn stats(State(api): State<Api>, identity: Identity) -> Result<Json<Stats>> {
    identity.require_read()?;
    let now = crate::now_millis();
    let started_at = api.started_at;
    let stats = api
        .db
        .call(move |conn| {
            let hour_ago = now - 60 * 60 * 1000;
            let one = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> crate::error::Result<i64> {
                Ok(conn.query_row(sql, params, |r| r.get(0))?)
            };
            Ok(Stats {
                queue: queue::depth(conn, now)?,
                apps: one("SELECT count(*) FROM apps", &[])?,
                endpoints: one("SELECT count(*) FROM endpoints", &[])?,
                endpoints_disabled: one(
                    "SELECT count(*) FROM endpoints WHERE disabled_at IS NOT NULL",
                    &[],
                )?,
                circuits_open: one(
                    "SELECT count(*) FROM endpoint_health WHERE circuit_open_until > ?1",
                    &[&now],
                )?,
                attempts_last_hour: one(
                    "SELECT count(*) FROM attempts WHERE created_at >= ?1",
                    &[&hour_ago],
                )?,
                successes_last_hour: one(
                    "SELECT count(*) FROM attempts WHERE created_at >= ?1 AND status = 'success'",
                    &[&hour_ago],
                )?,
                uptime_ms: now - started_at,
            })
        })
        .await?;
    Ok(Json(stats))
}
