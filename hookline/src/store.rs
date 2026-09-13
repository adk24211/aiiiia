//! Every query, in one place.
//!
//! These are synchronous and take a `&Connection` rather than the pool, so
//! that several of them compose inside one transaction: creating a message and
//! fanning it out to endpoints has to be atomic, and a function that grabs its
//! own connection can never be part of someone else's transaction.
//!
//! Callers reach them through [`crate::db::Db::call`], which supplies the
//! connection on a blocking thread.

use crate::error::{Error, Result};
use crate::ids::{self, Kind};
use crate::models::*;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// The largest page any listing will return, whatever the caller asks for.
pub const MAX_LIMIT: usize = 250;
pub const DEFAULT_LIMIT: usize = 50;

pub fn clamp_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

pub mod apps {
    use super::*;

    pub fn create(
        conn: &Connection,
        name: &str,
        uid: Option<&str>,
        metadata: &serde_json::Value,
        now: i64,
    ) -> Result<App> {
        let id = ids::new(Kind::Application);
        let metadata = metadata.to_string();
        conn.execute(
            "INSERT INTO apps(id, name, uid, metadata, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, uid, metadata, now],
        )
        .map_err(|e| unique_violation(e, "an application with that uid already exists"))?;
        get(conn, &id)
    }

    /// Look an application up by its id or by the uid you gave it.
    ///
    /// Both work in every route, so an integration can address tenants by the
    /// identifier it already has instead of storing a mapping to ours.
    pub fn get(conn: &Connection, id_or_uid: &str) -> Result<App> {
        conn.query_row(
            "SELECT * FROM apps WHERE id = ?1 OR uid = ?1",
            [id_or_uid],
            App::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::not_found("application"))
    }

    pub fn list(conn: &Connection, cursor: Option<&str>, limit: usize) -> Result<Page<App>> {
        let mut stmt =
            conn.prepare("SELECT * FROM apps WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2")?;
        let rows = stmt
            .query_map(params![cursor, limit as i64 + 1], App::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Page::build(rows, limit, |a| a.id.clone()))
    }

    pub fn update(
        conn: &Connection,
        id: &str,
        name: Option<&str>,
        uid: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<App> {
        let app = get(conn, id)?;
        let metadata = metadata.map(|m| m.to_string());
        conn.execute(
            "UPDATE apps SET name = COALESCE(?2, name),
                             uid = COALESCE(?3, uid),
                             metadata = COALESCE(?4, metadata)
             WHERE id = ?1",
            params![app.id, name, uid, metadata],
        )
        .map_err(|e| unique_violation(e, "an application with that uid already exists"))?;
        get(conn, &app.id)
    }

    /// Delete an application and, by cascade, everything under it.
    pub fn delete(conn: &Connection, id: &str) -> Result<()> {
        let app = get(conn, id)?;
        conn.execute("DELETE FROM apps WHERE id = ?1", [&app.id])?;
        Ok(())
    }
}

pub mod endpoints {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        conn: &Transaction<'_>,
        app_id: &str,
        url: &str,
        description: &str,
        event_types: Option<&[String]>,
        rate_limit: Option<u32>,
        secret: &str,
        now: i64,
    ) -> Result<(Endpoint, Secret)> {
        let id = ids::new(Kind::Endpoint);
        let types = event_types.map(|t| serde_json::to_string(t).unwrap_or_else(|_| "null".into()));
        conn.execute(
            "INSERT INTO endpoints(id, app_id, url, description, event_types, rate_limit,
                                   created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, app_id, url, description, types, rate_limit, now],
        )?;
        let secret = secrets::add(conn, &id, secret, now)?;
        Ok((get(conn, app_id, &id)?, secret))
    }

    /// Fetch an endpoint, refusing it if it belongs to another application.
    ///
    /// Every read takes the application it should belong to. A tenant scope
    /// that is checked in the handler is a tenant scope someone forgets to
    /// check; here it cannot be omitted without changing the signature.
    pub fn get(conn: &Connection, app_id: &str, id: &str) -> Result<Endpoint> {
        conn.query_row(
            "SELECT * FROM endpoints WHERE id = ?1 AND app_id = ?2",
            params![id, app_id],
            Endpoint::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::not_found("endpoint"))
    }

    pub fn list(
        conn: &Connection,
        app_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Endpoint>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM endpoints
             WHERE app_id = ?1 AND (?2 IS NULL OR id > ?2)
             ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![app_id, cursor, limit as i64 + 1],
                Endpoint::from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Page::build(rows, limit, |e| e.id.clone()))
    }

    /// Every endpoint of an application that is switched on. The fan-out reads
    /// this and filters in Rust rather than in SQL: the filter is a pattern
    /// language, and an application has tens of endpoints, not millions.
    pub fn active(conn: &Connection, app_id: &str) -> Result<Vec<Endpoint>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM endpoints WHERE app_id = ?1 AND disabled_at IS NULL ORDER BY id",
        )?;
        let rows = stmt
            .query_map([app_id], Endpoint::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        conn: &Connection,
        app_id: &str,
        id: &str,
        url: Option<&str>,
        description: Option<&str>,
        event_types: Option<Option<&[String]>>,
        rate_limit: Option<Option<u32>>,
        now: i64,
    ) -> Result<Endpoint> {
        let current = get(conn, app_id, id)?;
        // `Option<Option<_>>` distinguishes "leave it alone" from "set it to
        // null", which a single Option cannot: clearing a filter and not
        // mentioning it are different requests.
        let types = match event_types {
            None => current
                .event_types
                .as_ref()
                .map(|t| serde_json::to_string(t).unwrap_or_else(|_| "null".into())),
            Some(None) => None,
            Some(Some(t)) => Some(serde_json::to_string(t).unwrap_or_else(|_| "null".into())),
        };
        let rate = match rate_limit {
            None => current.rate_limit,
            Some(v) => v,
        };
        conn.execute(
            "UPDATE endpoints SET url = COALESCE(?3, url),
                                  description = COALESCE(?4, description),
                                  event_types = ?5,
                                  rate_limit = ?6,
                                  updated_at = ?7
             WHERE id = ?1 AND app_id = ?2",
            params![id, app_id, url, description, types, rate, now],
        )?;
        get(conn, app_id, id)
    }

    /// Switch an endpoint off. Pending deliveries to it are cancelled, because
    /// the alternative is a burst of traffic the moment it comes back.
    pub fn disable(
        conn: &Transaction<'_>,
        app_id: &str,
        id: &str,
        reason: &str,
        now: i64,
    ) -> Result<Endpoint> {
        get(conn, app_id, id)?;
        conn.execute(
            "UPDATE endpoints SET disabled_at = ?3, disabled_reason = ?4, updated_at = ?3
             WHERE id = ?1 AND app_id = ?2",
            params![id, app_id, now, reason],
        )?;
        conn.execute(
            "UPDATE deliveries SET status = 'cancelled', updated_at = ?2, last_error = ?3
             WHERE endpoint_id = ?1 AND status = 'pending'",
            params![id, now, "the endpoint was disabled"],
        )?;
        get(conn, app_id, id)
    }

    pub fn enable(conn: &Connection, app_id: &str, id: &str, now: i64) -> Result<Endpoint> {
        get(conn, app_id, id)?;
        conn.execute(
            "UPDATE endpoints SET disabled_at = NULL, disabled_reason = NULL, updated_at = ?3
             WHERE id = ?1 AND app_id = ?2",
            params![id, app_id, now],
        )?;
        // A re-enabled endpoint starts with a clean breaker: the failures that
        // opened it are about a state the operator says has changed.
        conn.execute("DELETE FROM endpoint_health WHERE endpoint_id = ?1", [id])?;
        get(conn, app_id, id)
    }

    pub fn delete(conn: &Connection, app_id: &str, id: &str) -> Result<()> {
        get(conn, app_id, id)?;
        conn.execute(
            "DELETE FROM endpoints WHERE id = ?1 AND app_id = ?2",
            params![id, app_id],
        )?;
        Ok(())
    }
}

pub mod secrets {
    use super::*;

    pub fn add(conn: &Connection, endpoint_id: &str, secret: &str, now: i64) -> Result<Secret> {
        let id = ids::new(Kind::Secret);
        conn.execute(
            "INSERT INTO endpoint_secrets(id, endpoint_id, secret, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, endpoint_id, secret, now],
        )?;
        conn.query_row(
            "SELECT * FROM endpoint_secrets WHERE id = ?1",
            [&id],
            Secret::from_row,
        )
        .map_err(Into::into)
    }

    /// Every secret of an endpoint, newest first.
    pub fn list(conn: &Connection, endpoint_id: &str) -> Result<Vec<Secret>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM endpoint_secrets WHERE endpoint_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([endpoint_id], Secret::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The secrets a request should be signed with right now.
    ///
    /// More than one during a rotation. Signing with all of them is what makes
    /// a rotation not an outage: the consumer accepts either until it has
    /// moved, and the old one expires on its own.
    pub fn active(conn: &Connection, endpoint_id: &str, now: i64) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT secret FROM endpoint_secrets
             WHERE endpoint_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)
             ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![endpoint_id, now], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Add a new secret and give every existing one a deadline.
    ///
    /// The old secrets keep signing until `grace_millis` have passed, so a
    /// consumer that stores one secret has that long to pick up the new one.
    pub fn rotate(
        conn: &Transaction<'_>,
        endpoint_id: &str,
        new_secret: &str,
        grace_millis: i64,
        now: i64,
    ) -> Result<Secret> {
        conn.execute(
            "UPDATE endpoint_secrets SET expires_at = ?2
             WHERE endpoint_id = ?1 AND expires_at IS NULL",
            params![endpoint_id, now + grace_millis],
        )?;
        add(conn, endpoint_id, new_secret, now)
    }

    /// Expire a secret immediately. For the case a secret has leaked, where a
    /// grace period is the wrong answer.
    ///
    /// The check runs before the write, not after: a guard that refuses once
    /// the row is already changed has enforced nothing.
    pub fn revoke(conn: &Connection, endpoint_id: &str, secret_id: &str, now: i64) -> Result<()> {
        let others: i64 = conn.query_row(
            "SELECT count(*) FROM endpoint_secrets
             WHERE endpoint_id = ?1 AND id <> ?2 AND (expires_at IS NULL OR expires_at > ?3)",
            params![endpoint_id, secret_id, now],
            |r| r.get(0),
        )?;
        if others == 0 {
            return Err(Error::conflict(
                "an endpoint must keep one signing secret; add another before revoking this one",
            ));
        }
        let changed = conn.execute(
            "UPDATE endpoint_secrets SET expires_at = ?3 WHERE id = ?1 AND endpoint_id = ?2",
            params![secret_id, endpoint_id, now],
        )?;
        if changed == 0 {
            return Err(Error::not_found("secret"));
        }
        Ok(())
    }
}

pub mod messages {
    use super::*;

    /// Store a message and queue it to every endpoint that wants it.
    ///
    /// Returns the message and the deliveries created. If an idempotency key
    /// was given and has been seen before, the stored message is returned
    /// unchanged and nothing new is queued — a retried POST must not fan out
    /// twice.
    pub fn create(
        tx: &Transaction<'_>,
        app_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
        idempotency_key: Option<&str>,
        now: i64,
    ) -> Result<(Message, Vec<Delivery>, bool)> {
        if let Some(key) = idempotency_key {
            if let Some(existing) = tx
                .query_row(
                    "SELECT * FROM messages WHERE app_id = ?1 AND idempotency_key = ?2",
                    params![app_id, key],
                    Message::from_row,
                )
                .optional()?
            {
                let deliveries = deliveries::for_message(tx, app_id, &existing.id)?;
                return Ok((existing, deliveries, true));
            }
        }

        let id = ids::new_at(Kind::Message, now);
        tx.execute(
            "INSERT INTO messages(id, app_id, event_type, payload, idempotency_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                app_id,
                event_type,
                payload.to_string(),
                idempotency_key,
                now
            ],
        )?;

        let mut created = Vec::new();
        for endpoint in endpoints::active(tx, app_id)? {
            if !endpoint.wants(event_type) {
                continue;
            }
            created.push(deliveries::enqueue(tx, &id, &endpoint.id, app_id, now)?);
        }

        let message = tx.query_row(
            "SELECT * FROM messages WHERE id = ?1",
            [&id],
            Message::from_row,
        )?;
        Ok((message, created, false))
    }

    pub fn get(conn: &Connection, app_id: &str, id: &str) -> Result<Message> {
        conn.query_row(
            "SELECT * FROM messages WHERE id = ?1 AND app_id = ?2",
            params![id, app_id],
            Message::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::not_found("message"))
    }

    /// Newest first, which is the order anyone debugging wants.
    pub fn list(
        conn: &Connection,
        app_id: &str,
        event_type: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Message>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM messages
             WHERE app_id = ?1
               AND (?2 IS NULL OR event_type = ?2)
               AND (?3 IS NULL OR id < ?3)
             ORDER BY id DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![app_id, event_type, cursor, limit as i64 + 1],
                Message::from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Page::build(rows, limit, |m| m.id.clone()))
    }
}

pub mod deliveries {
    use super::*;

    pub fn enqueue(
        conn: &Connection,
        message_id: &str,
        endpoint_id: &str,
        app_id: &str,
        now: i64,
    ) -> Result<Delivery> {
        let id = ids::new_at(Kind::Delivery, now);
        conn.execute(
            "INSERT INTO deliveries(id, message_id, endpoint_id, app_id, status, attempts,
                                    next_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?5, ?5)",
            params![id, message_id, endpoint_id, app_id, now],
        )?;
        get_unscoped(conn, &id)
    }

    pub fn get(conn: &Connection, app_id: &str, id: &str) -> Result<Delivery> {
        conn.query_row(
            "SELECT * FROM deliveries WHERE id = ?1 AND app_id = ?2",
            params![id, app_id],
            Delivery::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::not_found("delivery"))
    }

    /// For the workers, which have no tenant of their own.
    pub fn get_unscoped(conn: &Connection, id: &str) -> Result<Delivery> {
        conn.query_row(
            "SELECT * FROM deliveries WHERE id = ?1",
            [id],
            Delivery::from_row,
        )
        .optional()?
        .ok_or_else(|| Error::not_found("delivery"))
    }

    pub fn for_message(conn: &Connection, app_id: &str, message_id: &str) -> Result<Vec<Delivery>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM deliveries WHERE message_id = ?1 AND app_id = ?2 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![message_id, app_id], Delivery::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_for_endpoint(
        conn: &Connection,
        app_id: &str,
        endpoint_id: &str,
        status: Option<DeliveryStatus>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Delivery>> {
        let status = status.map(|s| s.as_str());
        let mut stmt = conn.prepare(
            "SELECT * FROM deliveries
             WHERE app_id = ?1 AND endpoint_id = ?2
               AND (?3 IS NULL OR status = ?3)
               AND (?4 IS NULL OR id < ?4)
             ORDER BY id DESC LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(
                params![app_id, endpoint_id, status, cursor, limit as i64 + 1],
                Delivery::from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Page::build(rows, limit, |d| d.id.clone()))
    }

    /// Queue a delivery again, whatever state it ended in.
    ///
    /// The attempt counter resets, so a replay gets the full retry schedule:
    /// a replay is a new decision to deliver, not a continuation of the one
    /// that ran out.
    pub fn replay(conn: &Connection, app_id: &str, id: &str, now: i64) -> Result<Delivery> {
        let delivery = get(conn, app_id, id)?;
        conn.execute(
            "UPDATE deliveries SET status = 'pending', attempts = 0, next_at = ?2,
                                   lease_until = NULL, last_error = NULL, updated_at = ?2
             WHERE id = ?1",
            params![delivery.id, now],
        )?;
        get(conn, app_id, id)
    }

    /// Stop retrying, without recording it as a failure of the endpoint.
    pub fn cancel(conn: &Connection, app_id: &str, id: &str, now: i64) -> Result<Delivery> {
        let delivery = get(conn, app_id, id)?;
        if delivery.status.is_terminal() {
            return Err(Error::conflict(format!(
                "this delivery already {}",
                delivery.status.as_str()
            )));
        }
        conn.execute(
            "UPDATE deliveries SET status = 'cancelled', lease_until = NULL, updated_at = ?2
             WHERE id = ?1",
            params![delivery.id, now],
        )?;
        get(conn, app_id, id)
    }
}

pub mod attempts {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        conn: &Connection,
        delivery: &Delivery,
        attempt_no: u32,
        status: &str,
        status_code: Option<u16>,
        error: Option<&str>,
        duration_ms: i64,
        response_snippet: Option<&str>,
        now: i64,
    ) -> Result<Attempt> {
        let id = ids::new_at(Kind::Attempt, now);
        conn.execute(
            "INSERT INTO attempts(id, delivery_id, endpoint_id, message_id, app_id, attempt_no,
                                  status, status_code, error, duration_ms, response_snippet,
                                  created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                delivery.id,
                delivery.endpoint_id,
                delivery.message_id,
                delivery.app_id,
                attempt_no,
                status,
                status_code,
                error,
                duration_ms,
                response_snippet,
                now
            ],
        )?;
        conn.query_row(
            "SELECT * FROM attempts WHERE id = ?1",
            [&id],
            Attempt::from_row,
        )
        .map_err(Into::into)
    }

    pub fn for_delivery(
        conn: &Connection,
        app_id: &str,
        delivery_id: &str,
    ) -> Result<Vec<Attempt>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM attempts WHERE delivery_id = ?1 AND app_id = ?2 ORDER BY attempt_no",
        )?;
        let rows = stmt
            .query_map(params![delivery_id, app_id], Attempt::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_for_endpoint(
        conn: &Connection,
        app_id: &str,
        endpoint_id: &str,
        status: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Attempt>> {
        let mut stmt = conn.prepare(
            "SELECT * FROM attempts
             WHERE app_id = ?1 AND endpoint_id = ?2
               AND (?3 IS NULL OR status = ?3)
               AND (?4 IS NULL OR id < ?4)
             ORDER BY id DESC LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(
                params![app_id, endpoint_id, status, cursor, limit as i64 + 1],
                Attempt::from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Page::build(rows, limit, |a| a.id.clone()))
    }

    /// Delete attempts older than `before`. The audit trail is the largest
    /// table by far and the one with a natural expiry.
    pub fn prune(conn: &Connection, before: i64) -> Result<usize> {
        Ok(conn.execute("DELETE FROM attempts WHERE created_at < ?1", [before])?)
    }
}

pub mod health {
    use super::*;

    pub fn get(conn: &Connection, endpoint_id: &str) -> Result<Health> {
        Ok(conn
            .query_row(
                "SELECT * FROM endpoint_health WHERE endpoint_id = ?1",
                [endpoint_id],
                Health::from_row,
            )
            .optional()?
            .unwrap_or(Health {
                endpoint_id: endpoint_id.to_string(),
                ..Health::default()
            }))
    }

    pub fn record_success(conn: &Connection, endpoint_id: &str, now: i64) -> Result<()> {
        conn.execute(
            "INSERT INTO endpoint_health(endpoint_id, consecutive_failures, circuit_open_until,
                                         last_success_at)
             VALUES (?1, 0, NULL, ?2)
             ON CONFLICT(endpoint_id) DO UPDATE SET consecutive_failures = 0,
                                                    circuit_open_until = NULL,
                                                    last_success_at = ?2",
            params![endpoint_id, now],
        )?;
        Ok(())
    }

    /// Count a failure and return the new consecutive-failure total.
    pub fn record_failure(conn: &Connection, endpoint_id: &str, now: i64) -> Result<u32> {
        conn.execute(
            "INSERT INTO endpoint_health(endpoint_id, consecutive_failures, last_failure_at)
             VALUES (?1, 1, ?2)
             ON CONFLICT(endpoint_id) DO UPDATE SET
                 consecutive_failures = consecutive_failures + 1,
                 last_failure_at = ?2",
            params![endpoint_id, now],
        )?;
        Ok(get(conn, endpoint_id)?.consecutive_failures)
    }

    pub fn open_circuit(conn: &Connection, endpoint_id: &str, until: i64) -> Result<()> {
        conn.execute(
            "UPDATE endpoint_health SET circuit_open_until = ?2 WHERE endpoint_id = ?1",
            params![endpoint_id, until],
        )?;
        Ok(())
    }

    /// Close the circuit and forget the failures that opened it.
    ///
    /// For an explicit replay. The breaker's state is an inference from past
    /// attempts, and someone asking for a replay is telling us something we
    /// cannot infer: that the thing those attempts failed against has been
    /// fixed. Without this, a replay after five failures is accepted, reports
    /// what it queued, and delivers nothing until the cooldown lapses.
    pub fn close_circuit(conn: &Connection, endpoint_id: &str) -> Result<()> {
        conn.execute(
            "UPDATE endpoint_health SET circuit_open_until = NULL, consecutive_failures = 0
             WHERE endpoint_id = ?1",
            [endpoint_id],
        )?;
        Ok(())
    }
}

pub mod keys {
    use super::*;

    pub fn create(
        conn: &Connection,
        name: &str,
        hash: &str,
        prefix: &str,
        scope: &str,
        now: i64,
    ) -> Result<ApiKey> {
        let id = ids::new(Kind::ApiKey);
        conn.execute(
            "INSERT INTO api_keys(id, name, hash, prefix, scope, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, name, hash, prefix, scope, now],
        )?;
        conn.query_row(
            "SELECT * FROM api_keys WHERE id = ?1",
            [&id],
            ApiKey::from_row,
        )
        .map_err(Into::into)
    }

    /// Find a key by the hash of the presented token.
    ///
    /// Revoked keys are excluded here rather than in the caller, so a revoked
    /// token cannot authenticate anything by way of a missed check.
    pub fn by_hash(conn: &Connection, hash: &str) -> Result<Option<ApiKey>> {
        Ok(conn
            .query_row(
                "SELECT * FROM api_keys WHERE hash = ?1 AND revoked_at IS NULL",
                [hash],
                ApiKey::from_row,
            )
            .optional()?)
    }

    pub fn touch(conn: &Connection, id: &str, now: i64) -> Result<()> {
        conn.execute(
            "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> Result<Vec<ApiKey>> {
        let mut stmt = conn.prepare("SELECT * FROM api_keys ORDER BY created_at DESC, id DESC")?;
        let rows = stmt
            .query_map([], ApiKey::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn revoke(conn: &Connection, id: &str, now: i64) -> Result<()> {
        let changed = conn.execute(
            "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, now],
        )?;
        if changed == 0 {
            return Err(Error::not_found("api key"));
        }
        Ok(())
    }

    /// Whether any key exists at all. A server with none is unconfigured, not
    /// open: the first key has to be minted from the command line.
    pub fn any(conn: &Connection) -> Result<bool> {
        let n: i64 = conn.query_row("SELECT count(*) FROM api_keys", [], |r| r.get(0))?;
        Ok(n > 0)
    }
}

/// Turn a unique-constraint violation into a 409 with a message a caller can
/// act on. Anything else passes through as the storage error it is.
fn unique_violation(e: rusqlite::Error, message: &str) -> Error {
    use rusqlite::ErrorCode;
    match &e {
        rusqlite::Error::SqliteFailure(err, _) if err.code == ErrorCode::ConstraintViolation => {
            Error::conflict(message.to_string())
        }
        _ => Error::from(e),
    }
}
