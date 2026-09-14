//! The delivery loop.
//!
//! One task claims work and hands it out; the deliveries themselves run
//! concurrently, bounded by a semaphore. The claim is deliberately *not*
//! concurrent: a single claimer writing one batch at a time is how SQLite
//! likes to be written to, and the concurrency that matters is in the HTTP
//! requests, which is where the time goes.
//!
//! The loop polls, but it is also woken directly when a message is posted, so
//! the poll interval is the latency floor for retries and for work another
//! process queued, not for the common case.

use crate::config::Config;
use crate::db::Db;
use crate::queue::{self, Job, Settled};
use crate::sender::{self, Sender};
use crate::{now_millis, store};
use std::sync::Arc;
use tokio::sync::{Notify, Semaphore};

/// A handle for telling the workers there is something to do.
///
/// Cloneable and cheap; the API holds one and pokes it after queueing a
/// message, which turns a poll interval of latency into none.
#[derive(Clone)]
pub struct Wake(Arc<Notify>);

impl Wake {
    pub fn new() -> Wake {
        Wake(Arc::new(Notify::new()))
    }

    /// Wake the claimer, if it is waiting.
    pub fn poke(&self) {
        self.0.notify_one();
    }
}

impl Default for Wake {
    fn default() -> Wake {
        Wake::new()
    }
}

/// Everything the loop needs.
#[derive(Clone)]
pub struct Workers {
    db: Db,
    sender: Sender,
    config: Arc<Config>,
    wake: Wake,
}

impl Workers {
    pub fn new(db: Db, config: Arc<Config>, wake: Wake) -> Result<Workers, String> {
        Ok(Workers {
            sender: Sender::new(&config)?,
            db,
            config,
            wake,
        })
    }

    /// Run until `shutdown` says otherwise.
    ///
    /// On shutdown the loop stops claiming and waits for what is in flight,
    /// because a delivery abandoned mid-request is one whose lease has to
    /// expire before anything happens to it — a minute of nothing, where
    /// waiting a few seconds costs nothing.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let permits = Arc::new(Semaphore::new(self.config.concurrency));
        let lease = self.config.lease.as_millis() as i64;

        loop {
            if *shutdown.borrow() {
                break;
            }

            // Never claim more than there is room to start: a claimed
            // delivery is leased, and one sitting in a local queue waiting for
            // a permit is a delivery nobody else may take either.
            let free = permits.available_permits();
            let batch = free.min(self.config.batch_size);
            let jobs = if batch == 0 {
                Vec::new()
            } else {
                match self.claim(batch, lease).await {
                    Ok(jobs) => jobs,
                    Err(e) => {
                        tracing::error!(error = %e, "could not claim deliveries");
                        Vec::new()
                    }
                }
            };

            if jobs.is_empty() {
                tokio::select! {
                    _ = self.wake.0.notified() => {}
                    _ = tokio::time::sleep(self.config.poll_interval) => {}
                    _ = shutdown.changed() => {}
                }
                continue;
            }

            for job in jobs {
                let permit = match Arc::clone(&permits).acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                let worker = self.clone();
                tokio::spawn(async move {
                    worker.deliver(job).await;
                    drop(permit);
                });
            }
        }

        // Wait for the in-flight deliveries by taking every permit.
        tracing::info!("draining in-flight deliveries");
        let _ = permits.acquire_many(self.config.concurrency as u32).await;
        tracing::info!("workers stopped");
    }

    async fn claim(&self, batch: usize, lease: i64) -> crate::error::Result<Vec<Job>> {
        self.db
            .call(move |conn| {
                let tx = crate::db::write_tx(conn)?;
                let jobs = queue::claim(&tx, now_millis(), lease, batch)?;
                tx.commit()?;
                Ok(jobs)
            })
            .await
    }

    /// Send one delivery and write down what happened.
    async fn deliver(&self, job: Job) {
        let outcome = self.sender.send(&job, now_millis()).await;
        let now = now_millis();

        // A non-retryable answer ends the delivery whatever the schedule says.
        let retry_at = if sender::is_retryable(&outcome) {
            self.config
                .retry
                .delay(job.delivery.attempts + 1)
                .map(|d| now + d.as_millis() as i64)
        } else {
            None
        };

        let policy = self.config.breaker.clone();
        let recorded = self
            .db
            .call(move |conn| {
                let tx = crate::db::write_tx(conn)?;
                let settled = queue::settle(&tx, &job, &outcome, retry_at, &policy, now)?;
                tx.commit()?;
                Ok((settled, job))
            })
            .await;

        match recorded {
            Ok((settled, job)) => log(&job, settled),
            Err(e) => {
                // The delivery was sent but the result could not be stored.
                // Nothing here can fix that; the lease expires and it is tried
                // again, which is the safe direction to fail in for a webhook
                // consumer that was asked to be idempotent.
                tracing::error!(error = %e, "could not record a delivery attempt");
            }
        }
    }
}

fn log(job: &Job, settled: Settled) {
    match settled {
        Settled::Superseded => tracing::info!(
            delivery = %job.delivery.id,
            endpoint = %job.endpoint.id,
            "the delivery was changed through the API while it was being sent"
        ),
        Settled::Succeeded => tracing::info!(
            delivery = %job.delivery.id,
            endpoint = %job.endpoint.id,
            attempt = job.delivery.attempts + 1,
            "delivered"
        ),
        Settled::Retrying { next_at } => tracing::info!(
            delivery = %job.delivery.id,
            endpoint = %job.endpoint.id,
            attempt = job.delivery.attempts + 1,
            retry_in_ms = next_at - now_millis(),
            "delivery failed, will retry"
        ),
        Settled::Failed => tracing::warn!(
            delivery = %job.delivery.id,
            endpoint = %job.endpoint.id,
            attempts = job.delivery.attempts + 1,
            "delivery failed for the last time"
        ),
        Settled::FailedAndDisabled => tracing::warn!(
            delivery = %job.delivery.id,
            endpoint = %job.endpoint.id,
            "delivery failed and the endpoint was disabled"
        ),
    }
}

/// Delete attempts older than the retention window, once an hour.
///
/// The attempt log is the largest table by a wide margin and the only one that
/// grows without bound, so the one piece of housekeeping this needs is here
/// rather than in a cron job the operator has to remember.
pub async fn prune(
    db: Db,
    retention: std::time::Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let every = std::time::Duration::from_secs(60 * 60);
    let window = retention.as_millis() as i64;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = shutdown.changed() => return,
        }
        if *shutdown.borrow() {
            return;
        }
        let before = now_millis() - window;
        match db
            .call(move |conn| store::attempts::prune(conn, before))
            .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "pruned old attempts"),
            Err(e) => tracing::error!(error = %e, "could not prune attempts"),
        }
    }
}

/// Put every expired lease back, once at startup.
///
/// A process that was killed leaves leases that would each have to time out on
/// their own. Clearing them when the replacement starts turns a minute of
/// stalled queue into none, and is safe precisely because the process that
/// held them is gone.
pub async fn reclaim_leases(db: Db) -> crate::error::Result<usize> {
    db.call(|conn| {
        Ok(conn.execute(
            "UPDATE deliveries SET lease_until = NULL
             WHERE status = 'pending' AND lease_until IS NOT NULL",
            [],
        )?)
    })
    .await
}
