//! Making one HTTP request, and deciding what its answer meant.
//!
//! The request is deliberately unadventurous. No redirects, because a public
//! URL that redirects to `127.0.0.1` is the SSRF hole that survives every
//! check made before the request. A hard timeout, because an endpoint that
//! accepts a connection and then says nothing would otherwise hold a worker
//! forever. A cap on how much of the response is read, because the audit trail
//! should not be a place a customer can write a megabyte per attempt into.
//!
//! The interesting decision is which answers are worth retrying. A 4xx is the
//! endpoint saying the request is wrong, and sending it again unchanged will
//! get the same answer; the two exceptions are 408 and 429, which are about
//! timing rather than content.

use crate::config::Config;
use crate::guard;
use crate::queue::{Job, Outcome};
use crate::sign;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The HTTP client, built once.
///
/// Once, because a client owns a connection pool and a TLS configuration:
/// building one per request would mean a fresh TLS handshake to every endpoint
/// every time, which is most of the cost of a small webhook.
#[derive(Clone)]
pub struct Sender {
    client: reqwest::Client,
    user_agent: String,
    max_payload_bytes: usize,
    max_response_snippet: usize,
    destinations: guard::Policy,
}

impl Sender {
    pub fn new(config: &Config) -> Result<Sender, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.request_timeout)
            .connect_timeout(config.request_timeout.min(Duration::from_secs(10)))
            // The environment's proxy variables are ignored on purpose: a
            // proxy resolves DNS itself, which would put the destination
            // policy below out of the loop entirely.
            .no_proxy()
            .dns_resolver(Arc::new(guard::Resolver::new(config.destinations.clone())))
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {}", e))?;
        Ok(Sender {
            client,
            user_agent: config.user_agent.clone(),
            max_payload_bytes: config.max_payload_bytes,
            max_response_snippet: config.max_response_snippet,
            destinations: config.destinations.clone(),
        })
    }

    /// Send one delivery and report what happened.
    ///
    /// Never returns an error: every way this can go wrong is a fact about the
    /// attempt that belongs in the audit trail, not an error for the caller to
    /// handle. A worker that has to decide what an error means is a worker
    /// that can drop a delivery.
    pub async fn send(&self, job: &Job, now: i64) -> Outcome {
        let started = Instant::now();

        // The shape of the URL is judged here as well as when the endpoint was
        // created: a policy can be tightened after an endpoint exists, and the
        // check that matters is the one at the moment of sending.
        if let Err(why) = guard::check_url(&job.endpoint.url, &self.destinations) {
            return Outcome {
                succeeded: false,
                status_code: None,
                error: Some(format!("refused: {}", why)),
                duration_ms: 0,
                response_snippet: None,
            };
        }

        let body = job.message.payload.to_string();
        if body.len() > self.max_payload_bytes {
            return Outcome {
                succeeded: false,
                status_code: None,
                error: Some(format!(
                    "the payload is {} bytes, over the {} byte limit",
                    body.len(),
                    self.max_payload_bytes
                )),
                duration_ms: 0,
                response_snippet: None,
            };
        }

        let timestamp = now / 1000;
        let signature = sign::sign(&job.secrets, &job.message.id, timestamp, body.as_bytes());

        let request = self
            .client
            .post(&job.endpoint.url)
            .header("content-type", "application/json")
            .header("user-agent", &self.user_agent)
            .header("webhook-id", &job.message.id)
            .header("webhook-timestamp", timestamp.to_string())
            .header("webhook-signature", signature)
            // Not part of the signature scheme, and not to be trusted by a
            // consumer for anything: they are here so a human reading their
            // own access log can tell a retry from a first attempt.
            .header("hookline-event-type", &job.message.event_type)
            .header("hookline-delivery-id", &job.delivery.id)
            .header("hookline-attempt", (job.delivery.attempts + 1).to_string())
            .body(body);

        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                return Outcome {
                    succeeded: false,
                    status_code: None,
                    error: Some(describe(&e)),
                    duration_ms: started.elapsed().as_millis() as i64,
                    response_snippet: None,
                }
            }
        };

        let status = response.status();
        let snippet = self.read_snippet(response).await;
        let duration_ms = started.elapsed().as_millis() as i64;

        Outcome {
            succeeded: status.is_success(),
            status_code: Some(status.as_u16()),
            error: (!status.is_success())
                .then(|| format!("the endpoint answered {}", status.as_u16())),
            duration_ms,
            response_snippet: snippet,
        }
    }

    /// Read at most `max_response_snippet` bytes of the body.
    ///
    /// Streamed and stopped early rather than read whole and truncated: an
    /// endpoint that answers with a gigabyte should cost us a few kilobytes of
    /// memory, not a gigabyte.
    async fn read_snippet(&self, mut response: reqwest::Response) -> Option<String> {
        if self.max_response_snippet == 0 {
            return None;
        }
        let mut buffer: Vec<u8> = Vec::new();
        while buffer.len() < self.max_response_snippet {
            match response.chunk().await {
                Ok(Some(chunk)) => buffer.extend_from_slice(&chunk),
                Ok(None) | Err(_) => break,
            }
        }
        buffer.truncate(self.max_response_snippet);
        // Lossy on purpose: this is for a human reading a failed attempt, and
        // an endpoint that answers with invalid UTF-8 should still leave
        // something legible behind.
        let text = String::from_utf8_lossy(&buffer).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

/// Whether an answer is worth trying again.
///
/// A 4xx means the request itself is wrong; sending the identical bytes again
/// will get the identical answer, and nine more attempts only add load to an
/// endpoint that has already said no. 408 and 429 are the exceptions: both are
/// about when the request arrived, not what was in it.
pub fn is_retryable(outcome: &Outcome) -> bool {
    match outcome.status_code {
        None => true,
        Some(code) => !(400..500).contains(&code) || code == 408 || code == 429,
    }
}

/// A short description of a transport failure, for the audit trail.
///
/// `reqwest`'s own `Display` is a chain ending in the cause, which reads badly
/// and can be long; what someone debugging wants is the category.
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "the request timed out".to_string();
    }
    if e.is_connect() {
        // The source carries the reason a connection failed, and for a
        // destination the resolver refused, that reason is the whole point.
        let mut source = e.source();
        while let Some(cause) = source {
            let text = cause.to_string();
            if text.contains("refused:") || text.contains("resolve") || text.contains("is ") {
                return format!("could not connect: {}", text);
            }
            source = cause.source();
        }
        return "could not connect".to_string();
    }
    if e.is_body() || e.is_decode() {
        return "the response could not be read".to_string();
    }
    if e.is_redirect() {
        return "the endpoint redirected, which is not followed".to_string();
    }
    let text = e.to_string();
    if text.len() > 200 {
        format!("{}...", &text[..200])
    } else {
        text
    }
}

use std::error::Error as _;

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(status_code: Option<u16>) -> Outcome {
        Outcome {
            succeeded: false,
            status_code,
            error: None,
            duration_ms: 0,
            response_snippet: None,
        }
    }

    #[test]
    fn a_transport_failure_is_retried() {
        assert!(is_retryable(&outcome(None)));
    }

    #[test]
    fn a_server_error_is_retried() {
        for code in [500, 502, 503, 504, 599] {
            assert!(is_retryable(&outcome(Some(code))), "{}", code);
        }
    }

    #[test]
    fn a_client_error_is_not_retried() {
        for code in [400, 401, 403, 404, 410, 422] {
            assert!(!is_retryable(&outcome(Some(code))), "{}", code);
        }
    }

    #[test]
    fn the_two_timing_answers_are_retried() {
        assert!(is_retryable(&outcome(Some(408))));
        assert!(is_retryable(&outcome(Some(429))));
    }

    #[test]
    fn a_client_can_be_built_from_the_defaults() {
        Sender::new(&Config::default()).expect("the default configuration must produce a client");
    }
}
