//! The queue: handing work to a worker and taking the result back.
//!
//! A worker claims a batch of due deliveries by writing a lease on them. The
//! lease is what makes a crash safe: a worker that dies mid-request leaves
//! rows whose lease simply expires, and the next claim picks them up. Nothing
//! has to notice the worker died, and no delivery is lost because the process
//! that owned it went away.
//!
//! The claim is a single statement. `SELECT` the due rows, `UPDATE` them and
//! `RETURNING` what was taken all happen inside one write, and SQLite
//! serialises writers, so two workers cannot claim the same delivery even
//! when they ask at the same instant.

use crate::breaker;
use crate::error::Result;
use crate::models::{Delivery, Endpoint, Message};
use crate::store;
use rusqlite::{params, Connection, Transaction};

/// How long a claimed delivery stays claimed.
///
/// Long enough to cover the request timeout with room to spare, because a
/// lease that expires while the request is still in flight means the same
/// webhook is sent twice.
pub const DEFAULT_LEASE: i64 = 60_000;

/// Everything a worker needs to send one webhook, read in one go.
///
/// The secrets travel with it because signing happens on the worker thread,
/// after the database connection has been given back: holding a connection
/// across an HTTP request would tie up the pool for the length of the slowest
/// endpoint any customer has.
#[derive(Debug, Clone)]
pub struct Job {
    pub delivery: Delivery,
    pub endpoint: Endpoint,
    pub message: Message,
    pub secrets: Vec<String>,
    /// The lease this worker was granted, as written at claim time.
    ///
    /// Carried so that [`settle`] can prove it is still the owner of the row
    /// it is about to write. Between the claim and the answer coming back, the
    /// API may have cancelled the delivery or replayed it, and a write keyed
    /// only on the id would silently undo whichever the operator asked for.
    pub lease_until: i64,
}

/// The period a per-endpoint rate limit is expressed over: `rate_limit`
/// deliveries per minute.
pub const RATE_PERIOD: i64 = 60_000;

/// Claim up to `limit` deliveries that are due, leasing them until
/// `now + lease_millis`.
///
/// Two passes, because rate-limited endpoints need different handling and
/// mixing them would make the common case pay for the rare one.
///
/// The first pass is the one almost every delivery goes through: a single
/// statement over the partial index on due deliveries. The select, the update
/// and what it returns are one write, and SQLite serialises writers, so two
/// workers cannot claim the same delivery.
///
/// The second pass takes at most one delivery from each rate-limited endpoint
/// that is allowed another, and moves that endpoint's next allowed time
/// forward by the spacing its limit implies. Keeping it out of the first pass
/// is what stops a rate-limited endpoint with a large backlog from sitting at
/// the front of the queue and starving everyone else.
pub fn claim(
    conn: &Transaction<'_>,
    now: i64,
    lease_millis: i64,
    limit: usize,
) -> Result<Vec<Job>> {
    let mut claimed = claim_unlimited(conn, now, lease_millis, limit)?;
    if claimed.len() < limit {
        claimed.extend(claim_limited(
            conn,
            now,
            lease_millis,
            limit - claimed.len(),
        )?);
    }

    let lease_until = now + lease_millis;
    let mut jobs = Vec::with_capacity(claimed.len());
    for delivery in claimed {
        let endpoint = store::endpoints::get(conn, &delivery.app_id, &delivery.endpoint_id)?;
        let message = store::messages::get(conn, &delivery.app_id, &delivery.message_id)?;
        let secrets = store::secrets::active(conn, &delivery.endpoint_id, now)?;
        jobs.push(Job {
            delivery,
            endpoint,
            message,
            secrets,
            lease_until,
        });
    }
    Ok(jobs)
}

/// The common path: endpoints with no limit of their own.
fn claim_unlimited(
    conn: &Transaction<'_>,
    now: i64,
    lease_millis: i64,
    limit: usize,
) -> Result<Vec<Delivery>> {
    let mut stmt = conn.prepare(
        "UPDATE deliveries SET lease_until = ?1 + ?2, updated_at = ?1
         WHERE id IN (
             SELECT d.id FROM deliveries d
             JOIN endpoints e ON e.id = d.endpoint_id
             WHERE d.status = 'pending'
               AND d.next_at <= ?1
               AND (d.lease_until IS NULL OR d.lease_until <= ?1)
               AND e.rate_limit IS NULL
               AND e.disabled_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM endpoint_health h
                   WHERE h.endpoint_id = d.endpoint_id AND h.circuit_open_until > ?1
               )
             ORDER BY d.next_at, d.id
             LIMIT ?3
         )
         RETURNING *",
    )?;
    let rows = stmt
        .query_map(params![now, lease_millis, limit as i64], Delivery::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One delivery per rate-limited endpoint that is due another.
fn claim_limited(
    conn: &Transaction<'_>,
    now: i64,
    lease_millis: i64,
    limit: usize,
) -> Result<Vec<Delivery>> {
    let ready: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            // Only endpoints that actually have work due. Without the
            // EXISTS, idle rate-limited endpoints fill the batch and one with
            // a queue behind it is never reached — the limit becomes a way for
            // quiet endpoints to starve a busy one.
            "SELECT e.id, e.rate_limit FROM endpoints e
             LEFT JOIN endpoint_health h ON h.endpoint_id = e.id
             WHERE e.rate_limit IS NOT NULL
               AND e.disabled_at IS NULL
               AND (h.circuit_open_until IS NULL OR h.circuit_open_until <= ?1)
               AND (h.next_allowed_at IS NULL OR h.next_allowed_at <= ?1)
               AND EXISTS (
                   SELECT 1 FROM deliveries d
                   WHERE d.endpoint_id = e.id
                     AND d.status = 'pending'
                     AND d.next_at <= ?1
                     AND (d.lease_until IS NULL OR d.lease_until <= ?1)
               )
             ORDER BY e.id
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![now, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut claimed = Vec::new();
    for (endpoint_id, rate_limit) in ready {
        let mut stmt = conn.prepare_cached(
            "UPDATE deliveries SET lease_until = ?1 + ?2, updated_at = ?1
             WHERE id = (
                 SELECT id FROM deliveries
                 WHERE endpoint_id = ?3
                   AND status = 'pending'
                   AND next_at <= ?1
                   AND (lease_until IS NULL OR lease_until <= ?1)
                 ORDER BY next_at, id
                 LIMIT 1
             )
             RETURNING *",
        )?;
        let taken = stmt
            .query_map(params![now, lease_millis, endpoint_id], Delivery::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        if taken.is_empty() {
            continue;
        }
        // Spacing rather than a bucket: at `rate_limit` per minute the next
        // one is allowed a minute divided by the limit from now, which makes
        // the rate exact and leaves no burst to absorb.
        //
        // The floor of one is not defensive tidiness. The API refuses a limit
        // of zero, but a row that carries one anyway must still be delivered
        // to slowly rather than parked for ever: a queue that silently never
        // drains is the worst of the available behaviours.
        let spacing = (RATE_PERIOD / rate_limit.max(1)).max(1);
        conn.execute(
            "INSERT INTO endpoint_health(endpoint_id, next_allowed_at) VALUES (?1, ?2)
             ON CONFLICT(endpoint_id) DO UPDATE SET next_allowed_at = ?2",
            params![endpoint_id, now + spacing],
        )?;
        claimed.extend(taken);
    }
    Ok(claimed)
}

/// How an attempt turned out, from the queue's point of view.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub succeeded: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub duration_ms: i64,
    pub response_snippet: Option<String>,
}

/// What happened to the delivery as a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// The API changed this delivery while the request was in flight, so the
    /// attempt is recorded and the operator's decision is left standing.
    Superseded,
    Succeeded,
    /// Another attempt is due at this time.
    Retrying {
        next_at: i64,
    },
    /// Out of attempts.
    Failed,
    /// Out of attempts, and the endpoint was switched off as well.
    FailedAndDisabled,
}

/// Record an attempt and move the delivery on.
///
/// `retry_at` is `None` when the schedule is exhausted. Everything here is one
/// transaction: the attempt row, the delivery's new state and the endpoint's
/// health have to agree, and a crash between them would leave a delivery that
/// is leased forever or a breaker counting failures that are not recorded.
pub fn settle(
    tx: &Transaction<'_>,
    job: &Job,
    outcome: &Outcome,
    retry_at: Option<i64>,
    policy: &breaker::Policy,
    now: i64,
) -> Result<Settled> {
    let attempt_no = job.delivery.attempts + 1;
    store::attempts::record(
        tx,
        &job.delivery,
        attempt_no,
        if outcome.succeeded {
            "success"
        } else {
            "failure"
        },
        outcome.status_code,
        outcome.error.as_deref(),
        outcome.duration_ms,
        outcome.response_snippet.as_deref(),
        now,
    )?;

    if outcome.succeeded {
        // The endpoint's health is about the endpoint, not about this row, so
        // it is recorded whether or not the delivery is still ours to write.
        store::health::record_success(tx, &job.endpoint.id, now)?;
        let changed = tx.execute(
            "UPDATE deliveries SET status = 'succeeded', attempts = ?2, lease_until = NULL,
                                   last_error = NULL, updated_at = ?3
             WHERE id = ?1 AND status = 'pending' AND attempts = ?4 AND lease_until = ?5",
            params![
                job.delivery.id,
                attempt_no,
                now,
                job.delivery.attempts,
                job.lease_until
            ],
        )?;
        if changed == 0 {
            return superseded(tx, job, now);
        }
        return Ok(Settled::Succeeded);
    }

    let consecutive = store::health::record_failure(tx, &job.endpoint.id, now)?;
    let action = policy.on_failure(consecutive, now);
    let error = outcome
        .error
        .clone()
        .unwrap_or_else(|| match outcome.status_code {
            Some(code) => format!("the endpoint answered {}", code),
            None => "the request failed".to_string(),
        });

    match retry_at {
        Some(next_at) => {
            // The circuit and the retry schedule both delay the next attempt;
            // whichever is later wins, so an open circuit is not defeated by a
            // delivery whose own backoff came due first.
            let next_at = match action {
                breaker::Action::Open { until } => next_at.max(until),
                _ => next_at,
            };
            let changed = tx.execute(
                "UPDATE deliveries SET attempts = ?2, next_at = ?3, lease_until = NULL,
                                       last_error = ?4, updated_at = ?5
                 WHERE id = ?1 AND status = 'pending' AND attempts = ?6
                   AND lease_until = ?7",
                params![
                    job.delivery.id,
                    attempt_no,
                    next_at,
                    error,
                    now,
                    job.delivery.attempts,
                    job.lease_until
                ],
            )?;
            apply(tx, job, action, now)?;
            if changed == 0 {
                return superseded(tx, job, now);
            }
            Ok(match action {
                breaker::Action::Disable => Settled::FailedAndDisabled,
                _ => Settled::Retrying { next_at },
            })
        }
        None => {
            let changed = tx.execute(
                "UPDATE deliveries SET status = 'failed', attempts = ?2, lease_until = NULL,
                                       last_error = ?3, updated_at = ?4
                 WHERE id = ?1 AND status = 'pending' AND attempts = ?5
                   AND lease_until = ?6",
                params![
                    job.delivery.id,
                    attempt_no,
                    error,
                    now,
                    job.delivery.attempts,
                    job.lease_until
                ],
            )?;
            apply(tx, job, action, now)?;
            if changed == 0 {
                return superseded(tx, job, now);
            }
            Ok(match action {
                breaker::Action::Disable => Settled::FailedAndDisabled,
                _ => Settled::Failed,
            })
        }
    }
}

/// The row moved on without us: the API cancelled, replayed, or re-queued this
/// delivery while the request was in flight.
///
/// The attempt is already recorded — it genuinely happened — and so is what it
/// said about the endpoint's health. What must not happen is writing a state
/// derived from a decision that predates the operator's, so the row is left
/// exactly as they set it. The lease is released only if we still hold it,
/// since by now it may belong to someone else.
fn superseded(tx: &Transaction<'_>, job: &Job, now: i64) -> Result<Settled> {
    tx.execute(
        "UPDATE deliveries SET lease_until = NULL, updated_at = ?3
         WHERE id = ?1 AND lease_until = ?2",
        params![job.delivery.id, job.lease_until, now],
    )?;
    tracing::info!(
        delivery = %job.delivery.id,
        "the delivery changed while it was being sent; the attempt stands, the row does not"
    );
    Ok(Settled::Superseded)
}

fn apply(tx: &Transaction<'_>, job: &Job, action: breaker::Action, now: i64) -> Result<()> {
    match action {
        breaker::Action::Continue => {}
        breaker::Action::Open { until } => {
            store::health::open_circuit(tx, &job.endpoint.id, until)?;
            tracing::warn!(
                endpoint = %job.endpoint.id,
                until,
                "circuit opened; deliveries to this endpoint are paused"
            );
        }
        breaker::Action::Disable => {
            store::endpoints::disable(
                tx,
                &job.endpoint.app_id,
                &job.endpoint.id,
                "disabled automatically after repeated failures",
                now,
            )?;
            tracing::warn!(endpoint = %job.endpoint.id, "endpoint disabled after repeated failures");
        }
    }
    Ok(())
}

/// Release a lease without recording an attempt.
///
/// For the case where a worker claimed a delivery and then could not try it at
/// all — the process is shutting down, say. The delivery goes straight back
/// into the queue with its attempt count untouched, because no request was
/// made and counting one would spend a retry on nothing.
pub fn release(conn: &Connection, delivery_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE deliveries SET lease_until = NULL, updated_at = ?2 WHERE id = ?1",
        params![delivery_id, now],
    )?;
    Ok(())
}

/// How much work is waiting, for the health endpoint and for metrics.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Depth {
    /// Deliveries not yet in a terminal state.
    pub pending: i64,
    /// Of those, the ones whose next attempt is already due.
    pub due: i64,
    /// Deliveries a worker is holding right now.
    pub in_flight: i64,
    /// How long the oldest due delivery has been waiting, in milliseconds.
    /// The number to alert on: a queue that is deep but moving is fine, and a
    /// queue that is shallow but stuck is not.
    pub oldest_due_age_ms: i64,
}

pub fn depth(conn: &Connection, now: i64) -> Result<Depth> {
    let (pending, due, in_flight, oldest): (i64, i64, i64, Option<i64>) = conn.query_row(
        "SELECT
            count(*),
            sum(CASE WHEN next_at <= ?1 THEN 1 ELSE 0 END),
            sum(CASE WHEN lease_until > ?1 THEN 1 ELSE 0 END),
            min(CASE WHEN next_at <= ?1 THEN next_at ELSE NULL END)
         FROM deliveries WHERE status = 'pending'",
        [now],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1).unwrap_or(0),
                r.get(2).unwrap_or(0),
                r.get(3)?,
            ))
        },
    )?;
    Ok(Depth {
        pending,
        due,
        in_flight,
        oldest_due_age_ms: oldest.map(|at| (now - at).max(0)).unwrap_or(0),
    })
}
