//! Configuration, and where it comes from.
//!
//! Every setting has a default that is right for a small deployment, so the
//! server starts with no configuration at all. Everything is overridable from
//! the environment, because that is what a container gives you. There is no
//! configuration file: a file is a third place for a setting to come from, and
//! the question "why is this value what it is" already has two answers.

use crate::backoff;
use crate::breaker;
use crate::guard;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Every knob, resolved.
#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub database: PathBuf,
    /// Connections in the pool. More than this many concurrent queries queue.
    pub pool_size: usize,

    /// How many deliveries may be in flight at once.
    pub concurrency: usize,
    /// How many deliveries one worker claims per pass.
    pub batch_size: usize,
    /// How long a claimed delivery stays claimed.
    pub lease: Duration,
    /// How long to wait before looking for work again when there was none.
    pub poll_interval: Duration,

    /// How long one delivery attempt may take, connection included.
    pub request_timeout: Duration,
    /// The largest request body that will be sent.
    pub max_payload_bytes: usize,
    /// How much of a response body is read and kept for the audit trail.
    pub max_response_snippet: usize,
    /// The `user-agent` sent with every delivery.
    pub user_agent: String,

    pub retry: backoff::Schedule,
    pub breaker: breaker::Policy,
    pub destinations: guard::Policy,

    /// How long a rotated secret keeps signing.
    pub secret_grace: Duration,
    /// Attempts older than this are deleted. `None` keeps them forever.
    pub attempt_retention: Option<Duration>,
    /// Serve the admin UI at `/`.
    pub admin_ui: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            listen: ([0, 0, 0, 0], 8080).into(),
            database: PathBuf::from("hookline.db"),
            pool_size: 8,
            concurrency: 32,
            batch_size: 16,
            lease: Duration::from_millis(crate::queue::DEFAULT_LEASE as u64),
            // Short enough that a webhook is not visibly late, long enough that
            // an idle deployment is not spinning. A message posted through the
            // API also wakes the workers, so this is the floor for retries and
            // for work another process queued, not for the common case.
            poll_interval: Duration::from_millis(500),
            request_timeout: Duration::from_secs(15),
            max_payload_bytes: 1024 * 1024,
            max_response_snippet: 2048,
            user_agent: concat!("hookline/", env!("CARGO_PKG_VERSION")).to_string(),
            retry: backoff::Schedule::default(),
            breaker: breaker::Policy::default(),
            destinations: guard::Policy::default(),
            secret_grace: Duration::from_secs(24 * 60 * 60),
            attempt_retention: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            admin_ui: true,
        }
    }
}

impl Config {
    /// Read the environment over the defaults.
    ///
    /// An unparseable value is an error rather than a warning and a default:
    /// a typo in `HOOKLINE_REQUEST_TIMEOUT` that silently leaves the timeout at
    /// fifteen seconds is a problem discovered in production.
    pub fn from_env() -> Result<Config, String> {
        let mut c = Config::default();

        if let Some(v) = var("HOOKLINE_LISTEN") {
            c.listen = v
                .parse()
                .map_err(|e| format!("HOOKLINE_LISTEN is not an address: {}", e))?;
        }
        if let Some(v) = var("HOOKLINE_DATABASE") {
            c.database = PathBuf::from(v);
        }
        c.pool_size = number("HOOKLINE_POOL_SIZE", c.pool_size)?;
        c.concurrency = number("HOOKLINE_CONCURRENCY", c.concurrency)?;
        c.batch_size = number("HOOKLINE_BATCH_SIZE", c.batch_size)?;
        c.max_payload_bytes = number("HOOKLINE_MAX_PAYLOAD_BYTES", c.max_payload_bytes)?;
        c.max_response_snippet = number("HOOKLINE_MAX_RESPONSE_SNIPPET", c.max_response_snippet)?;
        c.lease = seconds("HOOKLINE_LEASE_SECS", c.lease)?;
        c.poll_interval = millis("HOOKLINE_POLL_INTERVAL_MS", c.poll_interval)?;
        c.request_timeout = seconds("HOOKLINE_REQUEST_TIMEOUT_SECS", c.request_timeout)?;
        c.secret_grace = seconds("HOOKLINE_SECRET_GRACE_SECS", c.secret_grace)?;
        if let Some(v) = var("HOOKLINE_USER_AGENT") {
            c.user_agent = v;
        }
        if let Some(days) = var("HOOKLINE_ATTEMPT_RETENTION_DAYS") {
            let days: u64 = days
                .parse()
                .map_err(|_| "HOOKLINE_ATTEMPT_RETENTION_DAYS is not a number".to_string())?;
            c.attempt_retention = (days > 0).then(|| Duration::from_secs(days * 24 * 60 * 60));
        }

        c.retry.max_attempts = number("HOOKLINE_MAX_ATTEMPTS", c.retry.max_attempts)?;
        c.retry.base = seconds("HOOKLINE_RETRY_BASE_SECS", c.retry.base)?;
        c.retry.max_delay = seconds("HOOKLINE_RETRY_MAX_DELAY_SECS", c.retry.max_delay)?;
        c.breaker.failures_to_open =
            number("HOOKLINE_BREAKER_FAILURES", c.breaker.failures_to_open)?;
        c.breaker.cooldown = seconds("HOOKLINE_BREAKER_COOLDOWN_SECS", c.breaker.cooldown)?;
        if let Some(v) = var("HOOKLINE_BREAKER_DISABLE_AFTER") {
            let n: u32 = v
                .parse()
                .map_err(|_| "HOOKLINE_BREAKER_DISABLE_AFTER is not a number".to_string())?;
            c.breaker.failures_to_disable = (n > 0).then_some(n);
        }

        c.destinations.allow_http = flag("HOOKLINE_ALLOW_HTTP", c.destinations.allow_http)?;
        c.destinations.allow_private = flag(
            "HOOKLINE_ALLOW_PRIVATE_DESTINATIONS",
            c.destinations.allow_private,
        )?;
        if let Some(v) = var("HOOKLINE_ALLOWED_PORTS") {
            c.destinations.allowed_ports = v
                .split(',')
                .map(|p| p.trim().parse::<u16>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "HOOKLINE_ALLOWED_PORTS is not a list of ports".to_string())?;
        }
        if let Some(v) = var("HOOKLINE_DENIED_HOSTS") {
            c.destinations.denied_hosts = v
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect();
        }
        c.admin_ui = flag("HOOKLINE_ADMIN_UI", c.admin_ui)?;

        if c.concurrency == 0 {
            return Err("HOOKLINE_CONCURRENCY must be at least 1".into());
        }
        if c.pool_size == 0 {
            return Err("HOOKLINE_POOL_SIZE must be at least 1".into());
        }
        if c.lease <= c.request_timeout {
            return Err(format!(
                "the lease ({:?}) must outlast the request timeout ({:?}), or a delivery \
                 still in flight will be handed to a second worker and sent twice",
                c.lease, c.request_timeout
            ));
        }
        Ok(c)
    }

    /// A one-line summary for the log at startup, so that a support question
    /// about behaviour can be answered from the log rather than from a guess
    /// about what the environment held.
    pub fn summary(&self) -> String {
        format!(
            "listen={} database={} concurrency={} attempts={} timeout={:?} \
             allow_http={} allow_private={}",
            self.listen,
            self.database.display(),
            self.concurrency,
            self.retry.max_attempts,
            self.request_timeout,
            self.destinations.allow_http,
            self.destinations.allow_private,
        )
    }
}

fn var(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match var(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("{} is not a number: {:?}", name, v)),
    }
}

fn seconds(name: &str, default: Duration) -> Result<Duration, String> {
    Ok(Duration::from_secs(number(name, default.as_secs())?))
}

fn millis(name: &str, default: Duration) -> Result<Duration, String> {
    Ok(Duration::from_millis(number(
        name,
        default.as_millis() as u64,
    )?))
}

/// `1`, `true`, `yes` and `on` are true; `0`, `false`, `no` and `off` are
/// false. Anything else is an error, because a boolean that quietly reads
/// "maybe" as false is how a safety setting ends up off.
fn flag(name: &str, default: bool) -> Result<bool, String> {
    match var(name) {
        None => Ok(default),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(format!("{} is not a yes or no: {:?}", name, other)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_lease_outlasts_the_default_timeout() {
        // The invariant `from_env` enforces has to hold for the defaults too,
        // or the server refuses to start with no configuration at all.
        let c = Config::default();
        assert!(c.lease > c.request_timeout);
    }

    #[test]
    fn a_flag_reads_the_spellings_people_use() {
        for yes in ["1", "true", "TRUE", "yes", "on"] {
            std::env::set_var("HOOKLINE_TEST_FLAG", yes);
            assert_eq!(flag("HOOKLINE_TEST_FLAG", false), Ok(true), "{}", yes);
        }
        for no in ["0", "false", "NO", "off"] {
            std::env::set_var("HOOKLINE_TEST_FLAG", no);
            assert_eq!(flag("HOOKLINE_TEST_FLAG", true), Ok(false), "{}", no);
        }
        std::env::set_var("HOOKLINE_TEST_FLAG", "sure");
        assert!(flag("HOOKLINE_TEST_FLAG", false).is_err());
        std::env::remove_var("HOOKLINE_TEST_FLAG");
        assert_eq!(flag("HOOKLINE_TEST_FLAG", true), Ok(true));
    }

    #[test]
    fn an_unparseable_number_is_an_error_not_a_default() {
        std::env::set_var("HOOKLINE_TEST_NUMBER", "eight");
        assert!(number::<usize>("HOOKLINE_TEST_NUMBER", 8).is_err());
        std::env::remove_var("HOOKLINE_TEST_NUMBER");
    }
}
