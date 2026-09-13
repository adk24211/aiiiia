//! hookline — reliable webhook delivery.
//!
//! Sending a webhook is one line of code. Sending it *reliably* is a queue, a
//! retry schedule, a signature scheme, a circuit breaker, an audit trail and a
//! way to replay — and every team that needs webhooks writes a worse version
//! of all six. This is that, done once.
//!
//! The pieces are usable on their own: [`sign`] implements Standard Webhooks
//! signatures, [`guard`] validates a destination URL against the SSRF traps a
//! homegrown sender walks into, and [`backoff`] is the retry schedule.

#![forbid(unsafe_code)]

pub mod backoff;
pub mod db;
pub mod error;
pub mod guard;
pub mod ids;
pub mod models;
pub mod sign;
pub mod store;

/// Milliseconds since the Unix epoch.
///
/// Every timestamp stored or compared is this: one integer, one unit, no time
/// zone, sortable as a number. A duration between two of them is subtraction.
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
