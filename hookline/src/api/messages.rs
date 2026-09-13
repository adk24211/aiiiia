//! Sending events.
//!
//! This is the route that matters: everything else is configuration, and this
//! is the one an application server calls in the hot path of its own request.
//! It does exactly two writes and returns.

use super::{app_id, text, Api, Paging};
use crate::auth::Identity;
use crate::error::{Error, Result};
use crate::models::{Delivery, Message, Page};
use crate::store;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct New {
    /// What happened, as `noun.verb`: `invoice.paid`, `user.created`.
    pub event_type: String,
    /// The body that will be sent, verbatim, and signed as sent.
    pub payload: serde_json::Value,
    /// Post the same key twice and the second call changes nothing and returns
    /// the first message. May also be given as an `idempotency-key` header.
    pub idempotency_key: Option<String>,
}

#[derive(Serialize)]
pub struct Sent {
    #[serde(flatten)]
    pub message: Message,
    /// One per endpoint the message was queued to. Empty means no endpoint
    /// subscribes to this event type, which is worth noticing.
    pub deliveries: Vec<Delivery>,
    /// True when this was a repeat of a message already sent under the same
    /// idempotency key, and nothing new was queued.
    pub duplicate: bool,
}

pub async fn create(
    State(api): State<Api>,
    identity: Identity,
    Path(app): Path<String>,
    headers: HeaderMap,
    Json(body): Json<New>,
) -> Result<(axum::http::StatusCode, Json<Sent>)> {
    identity.require_publish()?;
    let app_id = app_id(&api.db, &app).await?;
    let event_type = text("event_type", &body.event_type, 200)?;

    let encoded = body.payload.to_string();
    if encoded.len() > api.config.max_payload_bytes {
        return Err(Error::invalid(format!(
            "the payload is {} bytes, over the {} byte limit",
            encoded.len(),
            api.config.max_payload_bytes
        )));
    }

    let idempotency_key = match body.idempotency_key {
        Some(key) => Some(text("idempotency_key", &key, 200)?),
        None => headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string()),
    };

    let payload = body.payload;
    let now = crate::now_millis();
    let (message, deliveries, duplicate) = api
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let out = store::messages::create(
                &tx,
                &app_id,
                &event_type,
                &payload,
                idempotency_key.as_deref(),
                now,
            )?;
            tx.commit()?;
            Ok(out)
        })
        .await?;

    if !duplicate && !deliveries.is_empty() {
        // Tell the workers rather than letting them find out on the next poll.
        api.wake.poke();
    }

    let status = if duplicate {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::ACCEPTED
    };
    Ok((
        status,
        Json(Sent {
            message,
            deliveries,
            duplicate,
        }),
    ))
}

#[derive(Deserialize, Default)]
pub struct Filter {
    pub event_type: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

pub async fn list(
    State(api): State<Api>,
    identity: Identity,
    Path(app): Path<String>,
    Query(filter): Query<Filter>,
) -> Result<Json<Page<Message>>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let limit = filter.paging.limit();
    let cursor = filter.paging.cursor;
    let event_type = filter.event_type;
    let page = api
        .db
        .call(move |conn| {
            store::messages::list(
                conn,
                &app_id,
                event_type.as_deref(),
                cursor.as_deref(),
                limit,
            )
        })
        .await?;
    Ok(Json(page))
}

pub async fn get(
    State(api): State<Api>,
    identity: Identity,
    Path((app, message)): Path<(String, String)>,
) -> Result<Json<Message>> {
    identity.require_read()?;
    let app_id = app_id(&api.db, &app).await?;
    let found = api
        .db
        .call(move |conn| store::messages::get(conn, &app_id, &message))
        .await?;
    Ok(Json(found))
}
