//! The HTTP API.
//!
//! Every route is under `/v1`, takes a bearer token, and answers JSON. The
//! shape is the same throughout: a single object for one thing, and
//! `{data, next_cursor, has_more}` for many.
//!
//! Tenant scoping is in the URL rather than in a header or the token, so a
//! request that reads the wrong application's data is a request to a different
//! path, which is visible in a log and impossible to do by forgetting a
//! filter.

mod apps;
mod deliveries;
mod endpoints;
mod keys;
mod messages;
mod system;

use crate::auth::{self, Identity};
use crate::config::Config;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::store;
use crate::worker::Wake;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use axum::Router;
use serde::Deserialize;
use std::sync::Arc;

/// What every handler is given.
#[derive(Clone)]
pub struct Api {
    pub db: Db,
    pub config: Arc<Config>,
    pub wake: Wake,
    pub started_at: i64,
}

pub fn router(state: Api) -> Router {
    let v1 = Router::new()
        .route("/apps", post(apps::create).get(apps::list))
        .route("/apps/:app", get(apps::get).delete(apps::delete))
        .route("/apps/:app", patch(apps::update))
        .route(
            "/apps/:app/endpoints",
            post(endpoints::create).get(endpoints::list),
        )
        .route(
            "/apps/:app/endpoints/:endpoint",
            get(endpoints::get)
                .patch(endpoints::update)
                .delete(endpoints::delete),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/disable",
            post(endpoints::disable),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/enable",
            post(endpoints::enable),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/health",
            get(endpoints::health),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/secrets",
            get(endpoints::secrets),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/secrets/rotate",
            post(endpoints::rotate),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/secrets/:secret",
            delete(endpoints::revoke),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/deliveries",
            get(deliveries::for_endpoint),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/attempts",
            get(deliveries::attempts_for_endpoint),
        )
        .route(
            "/apps/:app/endpoints/:endpoint/replay",
            post(deliveries::replay_endpoint),
        )
        .route(
            "/apps/:app/messages",
            post(messages::create).get(messages::list),
        )
        .route("/apps/:app/messages/:message", get(messages::get))
        .route(
            "/apps/:app/messages/:message/deliveries",
            get(deliveries::for_message),
        )
        .route("/apps/:app/deliveries/:delivery", get(deliveries::get))
        .route(
            "/apps/:app/deliveries/:delivery/attempts",
            get(deliveries::attempts),
        )
        .route(
            "/apps/:app/deliveries/:delivery/replay",
            post(deliveries::replay),
        )
        .route(
            "/apps/:app/deliveries/:delivery/cancel",
            post(deliveries::cancel),
        )
        .route("/keys", post(keys::create).get(keys::list))
        .route("/keys/:key", delete(keys::revoke))
        .route("/stats", get(system::stats))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ));

    Router::new()
        // Unauthenticated on purpose: a health check that needs a credential
        // is a health check that a load balancer cannot make.
        .route("/health", get(system::health))
        .route("/version", get(system::version))
        .nest("/v1", v1)
        .fallback(not_found)
        .with_state(state)
}

/// Reject anything without a valid token before it reaches a handler.
async fn require_token(
    State(api): State<Api>,
    mut request: Request,
    next: Next,
) -> std::result::Result<Response, Error> {
    let token = auth::token_from_headers(request.headers()).ok_or(Error::Unauthorized)?;
    let identity = auth::authenticate(&api.db, &token).await?;
    request.extensions_mut().insert(identity);
    Ok(next.run(request).await)
}

/// The identity the middleware established, for handlers to ask about scope.
#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Identity {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Identity, Error> {
        parts
            .extensions
            .get::<Identity>()
            .cloned()
            .ok_or(Error::Unauthorized)
    }
}

async fn not_found() -> Error {
    Error::NotFound("route".into())
}

/// The query parameters every listing accepts.
#[derive(Debug, Deserialize, Default)]
pub struct Paging {
    pub cursor: Option<String>,
    #[serde(default, deserialize_with = "number_or_text")]
    pub limit: Option<usize>,
}

impl Paging {
    pub fn limit(&self) -> usize {
        store::clamp_limit(self.limit)
    }
}

/// Read a number that may arrive as text.
///
/// A query string has no types, and `#[serde(flatten)]` hands the flattened
/// fields on as the strings they arrived as — so a plain `Option<usize>` here
/// parses `?limit=2` on a struct without a flattened filter and rejects it on
/// one with. Accepting both spellings is the only way the same parameter
/// behaves the same way on every listing.
fn number_or_text<'de, D>(deserializer: D) -> std::result::Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;

    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Option<usize>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a whole number")
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Option<usize>, E> {
            Ok(Some(v as usize))
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Option<usize>, E> {
            usize::try_from(v)
                .map(Some)
                .map_err(|_| E::custom("must not be negative"))
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Option<usize>, E> {
            if v.is_empty() {
                return Ok(None);
            }
            v.parse()
                .map(Some)
                .map_err(|_| E::custom(format!("{:?} is not a whole number", v)))
        }

        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Option<usize>, E> {
            Ok(None)
        }

        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Option<usize>, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> std::result::Result<Option<usize>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(Visitor)
        }
    }

    deserializer.deserialize_any(Visitor)
}

/// Resolve an application reference to its id, so every handler below it works
/// with the same thing whether the caller used our id or their own uid.
pub async fn app_id(db: &Db, reference: &str) -> Result<String> {
    let reference = reference.to_string();
    Ok(db
        .call(move |conn| store::apps::get(conn, &reference))
        .await?
        .id)
}

/// Reject a body field that is empty or absurdly long before it is stored.
pub fn text(field: &str, value: &str, max: usize) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid(format!("{} must not be empty", field)));
    }
    if trimmed.len() > max {
        return Err(Error::invalid(format!(
            "{} is {} bytes, over the {} byte limit",
            field,
            trimmed.len(),
            max
        )));
    }
    Ok(trimmed.to_string())
}
