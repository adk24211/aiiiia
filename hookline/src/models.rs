//! The domain types, and the JSON they are.
//!
//! These types are the API. A field added here appears in responses, so they
//! are written to be read by someone integrating against them: no internal
//! bookkeeping, no columns that only the workers care about, and timestamps
//! that are milliseconds since the epoch everywhere.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// A tenant. Everything else belongs to one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    pub id: String,
    pub name: String,
    /// Your own identifier for this tenant, if you gave one. Unique, and
    /// usable in place of `id` in every route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: i64,
}

impl App {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<App> {
        let metadata: String = row.get("metadata")?;
        Ok(App {
            id: row.get("id")?,
            name: row.get("name")?,
            uid: row.get("uid")?,
            metadata: serde_json::from_str(&metadata).unwrap_or(serde_json::Value::Null),
            created_at: row.get("created_at")?,
        })
    }
}

/// A destination URL, with the filters and limits that apply to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: String,
    pub app_id: String,
    pub url: String,
    pub description: String,
    /// The event types this endpoint wants, or `null` for all of them.
    pub event_types: Option<Vec<String>>,
    /// Set when the endpoint was switched off, by a human or by the breaker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Deliveries per minute, or `null` for no limit of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Endpoint {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Endpoint> {
        let event_types: Option<String> = row.get("event_types")?;
        Ok(Endpoint {
            id: row.get("id")?,
            app_id: row.get("app_id")?,
            url: row.get("url")?,
            description: row.get("description")?,
            event_types: event_types.and_then(|s| serde_json::from_str(&s).ok()),
            disabled_at: row.get("disabled_at")?,
            disabled_reason: row.get("disabled_reason")?,
            rate_limit: row.get::<_, Option<i64>>("rate_limit")?.map(|n| n as u32),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    /// Whether this endpoint should receive `event_type`.
    ///
    /// An endpoint with no filter takes everything; that is the default
    /// because the alternative — a new event type silently reaching nobody —
    /// is the failure people spend an afternoon debugging.
    pub fn wants(&self, event_type: &str) -> bool {
        match &self.event_types {
            None => true,
            Some(types) => types.iter().any(|t| matches_pattern(t, event_type)),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }
}

/// Whether a subscription pattern covers an event type.
///
/// A trailing `*` matches any suffix, so `invoice.*` covers `invoice.paid`.
/// That is the whole pattern language: anything richer invites filters nobody
/// can predict the behaviour of, and the answer to those is a filter in the
/// consumer.
pub fn matches_pattern(pattern: &str, event_type: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => event_type.starts_with(prefix),
        None => pattern == event_type,
    }
}

/// A secret an endpoint's requests are signed with.
///
/// The secret itself is only returned to the operator who owns the endpoint;
/// it is not in any listing that crosses a tenant boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secret {
    pub id: String,
    pub endpoint_id: String,
    pub secret: String,
    pub created_at: i64,
    /// When this secret stops being used for signing. A rotation sets it on
    /// the old secret rather than deleting it, so consumers have a window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl Secret {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Secret> {
        Ok(Secret {
            id: row.get("id")?,
            endpoint_id: row.get("endpoint_id")?,
            secret: row.get("secret")?,
            created_at: row.get("created_at")?,
            expires_at: row.get("expires_at")?,
        })
    }

    pub fn is_active(&self, now: i64) -> bool {
        self.expires_at.is_none_or(|at| at > now)
    }
}

/// An event you sent. Fans out to one delivery per matching endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub app_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    pub created_at: i64,
}

impl Message {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
        let payload: String = row.get("payload")?;
        Ok(Message {
            id: row.get("id")?,
            app_id: row.get("app_id")?,
            event_type: row.get("event_type")?,
            payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
            idempotency_key: row.get("idempotency_key")?,
            created_at: row.get("created_at")?,
        })
    }
}

/// Where one (message, endpoint) pair has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    /// Waiting for its next attempt.
    Pending,
    /// The endpoint accepted it.
    Succeeded,
    /// Out of attempts.
    Failed,
    /// Given up on deliberately: the endpoint was deleted or disabled.
    Cancelled,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryStatus::Pending => "pending",
            DeliveryStatus::Succeeded => "succeeded",
            DeliveryStatus::Failed => "failed",
            DeliveryStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Result<DeliveryStatus> {
        match s {
            "pending" => Ok(DeliveryStatus::Pending),
            "succeeded" => Ok(DeliveryStatus::Succeeded),
            "failed" => Ok(DeliveryStatus::Failed),
            "cancelled" => Ok(DeliveryStatus::Cancelled),
            other => Err(Error::invalid(format!("unknown status {:?}", other))),
        }
    }

    /// Whether anything more will happen to a delivery in this state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, DeliveryStatus::Pending)
    }
}

impl rusqlite::ToSql for DeliveryStatus {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub message_id: String,
    pub endpoint_id: String,
    pub app_id: String,
    pub status: DeliveryStatus,
    /// How many HTTP requests have been made so far.
    pub attempts: u32,
    /// When the next attempt is due. In the past means "now".
    pub next_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Delivery {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Delivery> {
        let status: String = row.get("status")?;
        Ok(Delivery {
            id: row.get("id")?,
            message_id: row.get("message_id")?,
            endpoint_id: row.get("endpoint_id")?,
            app_id: row.get("app_id")?,
            status: DeliveryStatus::parse(&status).unwrap_or(DeliveryStatus::Pending),
            attempts: row.get::<_, i64>("attempts")? as u32,
            next_at: row.get("next_at")?,
            last_error: row.get("last_error")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// One HTTP request, recorded whether it worked or not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub delivery_id: String,
    pub endpoint_id: String,
    pub message_id: String,
    pub app_id: String,
    pub attempt_no: u32,
    /// `success` or `failure`.
    pub status: String,
    /// Absent when the request never got a response at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: i64,
    /// The first few hundred bytes of the response body. Truncated on the way
    /// in, because an endpoint that returns a megabyte of HTML on error should
    /// not be able to fill the disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_snippet: Option<String>,
    pub created_at: i64,
}

impl Attempt {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
        Ok(Attempt {
            id: row.get("id")?,
            delivery_id: row.get("delivery_id")?,
            endpoint_id: row.get("endpoint_id")?,
            message_id: row.get("message_id")?,
            app_id: row.get("app_id")?,
            attempt_no: row.get::<_, i64>("attempt_no")? as u32,
            status: row.get("status")?,
            status_code: row.get::<_, Option<i64>>("status_code")?.map(|c| c as u16),
            error: row.get("error")?,
            duration_ms: row.get("duration_ms")?,
            response_snippet: row.get("response_snippet")?,
            created_at: row.get("created_at")?,
        })
    }
}

/// What the circuit breaker knows about an endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    pub endpoint_id: String,
    pub consecutive_failures: u32,
    /// While this is in the future, deliveries to the endpoint are deferred
    /// rather than attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circuit_open_until: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<i64>,
}

impl Health {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Health> {
        Ok(Health {
            endpoint_id: row.get("endpoint_id")?,
            consecutive_failures: row.get::<_, i64>("consecutive_failures")? as u32,
            circuit_open_until: row.get("circuit_open_until")?,
            last_success_at: row.get("last_success_at")?,
            last_failure_at: row.get("last_failure_at")?,
        })
    }
}

/// An API credential. The token itself exists only in the response that
/// created it; what is stored is a hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    /// The first few characters of the token, so a human can tell two keys
    /// apart in a list without either being recoverable from it.
    pub prefix: String,
    pub scope: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

impl ApiKey {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKey> {
        Ok(ApiKey {
            id: row.get("id")?,
            name: row.get("name")?,
            prefix: row.get("prefix")?,
            scope: row.get("scope")?,
            created_at: row.get("created_at")?,
            last_used_at: row.get("last_used_at")?,
            revoked_at: row.get("revoked_at")?,
        })
    }
}

/// A page of results, and the cursor for the next one.
///
/// Cursors are ids rather than offsets. Ids are sortable and stable, so a page
/// boundary does not move when a row is inserted, and `LIMIT ... OFFSET n`
/// never has to count past rows it will not return.
#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl<T> Page<T> {
    /// Build a page from `limit + 1` rows: the extra row is not returned, it
    /// is only how we know whether there is more.
    pub fn build(mut rows: Vec<T>, limit: usize, cursor_of: impl Fn(&T) -> String) -> Page<T> {
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if has_more {
            rows.last().map(&cursor_of)
        } else {
            None
        };
        Page {
            data: rows,
            next_cursor,
            has_more,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_without_a_star_is_exact() {
        assert!(matches_pattern("invoice.paid", "invoice.paid"));
        assert!(!matches_pattern("invoice.paid", "invoice.paid.late"));
        assert!(!matches_pattern("invoice", "invoice.paid"));
    }

    #[test]
    fn a_trailing_star_matches_a_prefix() {
        assert!(matches_pattern("invoice.*", "invoice.paid"));
        assert!(matches_pattern("invoice.*", "invoice.refunded"));
        assert!(!matches_pattern("invoice.*", "payment.paid"));
        assert!(matches_pattern("*", "anything.at.all"));
    }

    #[test]
    fn an_endpoint_without_filters_takes_everything() {
        let mut ep = endpoint(None);
        assert!(ep.wants("anything"));
        ep.event_types = Some(vec![]);
        assert!(
            !ep.wants("anything"),
            "an empty list is a filter, not the absence of one"
        );
    }

    #[test]
    fn a_page_reports_more_without_returning_it() {
        let rows: Vec<String> = (0..4).map(|i| format!("id{}", i)).collect();
        let page = Page::build(rows, 3, |s| s.clone());
        assert_eq!(page.data.len(), 3);
        assert!(page.has_more);
        assert_eq!(page.next_cursor.as_deref(), Some("id2"));

        let page = Page::build(vec!["a".to_string()], 3, |s| s.clone());
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn an_expiring_secret_stays_active_until_it_expires() {
        let mut s = Secret {
            id: "sec_1".into(),
            endpoint_id: "ep_1".into(),
            secret: "whsec_x".into(),
            created_at: 0,
            expires_at: None,
        };
        assert!(s.is_active(10_000));
        s.expires_at = Some(10_000);
        assert!(s.is_active(9_999));
        assert!(
            !s.is_active(10_000),
            "a secret is not active at the instant it expires"
        );
    }

    fn endpoint(event_types: Option<Vec<String>>) -> Endpoint {
        Endpoint {
            id: "ep_1".into(),
            app_id: "app_1".into(),
            url: "https://example.com/hook".into(),
            description: String::new(),
            event_types,
            disabled_at: None,
            disabled_reason: None,
            rate_limit: None,
            created_at: 0,
            updated_at: 0,
        }
    }
}
