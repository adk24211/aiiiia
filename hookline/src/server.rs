//! Starting everything and stopping it cleanly.

use crate::api;
use crate::config::Config;
use crate::db::Db;
use crate::worker::{self, Wake, Workers};
use std::sync::Arc;

/// A running server: the HTTP listener, the delivery workers, and the
/// housekeeping, all stopped together.
pub struct Server {
    pub config: Arc<Config>,
    pub db: Db,
    pub wake: Wake,
}

impl Server {
    pub fn new(config: Config) -> Result<Server, String> {
        let db = Db::open(&config.database, config.pool_size)
            .map_err(|e| format!("cannot open {}: {}", config.database.display(), e))?;
        Ok(Server {
            config: Arc::new(config),
            db,
            wake: Wake::new(),
        })
    }

    /// Serve until the process is asked to stop.
    pub async fn run(self) -> Result<(), String> {
        let listener = tokio::net::TcpListener::bind(self.config.listen)
            .await
            .map_err(|e| format!("cannot listen on {}: {}", self.config.listen, e))?;
        self.run_on(listener, signal()).await
    }

    /// Serve on a listener that is already bound, stopping when `shutdown`
    /// resolves.
    ///
    /// The test suite runs the whole server this way on an ephemeral port, so
    /// that what it exercises is this code and not a second wiring of it.
    pub async fn run_on(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), String> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Leases held by the process this one is replacing would otherwise
        // each have to time out on their own.
        match worker::reclaim_leases(self.db.clone()).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(reclaimed = n, "released leases from a previous run"),
            Err(e) => tracing::warn!(error = %e, "could not release stale leases"),
        }

        let workers = Workers::new(self.db.clone(), Arc::clone(&self.config), self.wake.clone())?;
        let worker_task = tokio::spawn(workers.run(shutdown_rx.clone()));

        let prune_task = self.config.attempt_retention.map(|retention| {
            tokio::spawn(worker::prune(
                self.db.clone(),
                retention,
                shutdown_rx.clone(),
            ))
        });

        let app = api::router(api::Api {
            db: self.db.clone(),
            config: Arc::clone(&self.config),
            wake: self.wake.clone(),
            started_at: crate::now_millis(),
        });

        let bound = listener.local_addr().unwrap_or(self.config.listen);
        tracing::info!(address = %bound, "listening");

        let serving = axum::serve(listener, app).with_graceful_shutdown(async move {
            shutdown.await;
            tracing::info!("shutting down");
        });
        let result = serving
            .await
            .map_err(|e| format!("the server stopped: {}", e));

        // The listener has stopped accepting; now stop the workers and let
        // the deliveries already in flight finish.
        let _ = shutdown_tx.send(true);
        let _ = worker_task.await;
        if let Some(task) = prune_task {
            task.abort();
        }
        result
    }
}

/// Wait for the operating system to ask the process to stop.
///
/// Both signals, because `SIGINT` is a terminal and `SIGTERM` is every
/// container runtime, and a server that only handles one of them appears to
/// hang for thirty seconds on every deploy.
async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
