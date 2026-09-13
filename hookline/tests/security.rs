//! The destination policy, from the outside.
//!
//! A webhook sender makes requests to addresses its users choose, from inside
//! your network. These are the requests it must refuse, checked against the
//! real server with the real default configuration — because the unit tests in
//! `guard` prove the predicate and these prove it is actually wired in.

mod support;

use hookline::config::Config;
use std::time::Duration;
use support::{Consumer, Harness};

/// A server with the shipped defaults, which refuse private destinations.
async fn strict() -> Harness {
    Harness::start_with(|c| Config {
        destinations: Default::default(),
        ..c
    })
    .await
}

#[tokio::test]
async fn the_cloud_metadata_service_cannot_be_reached() {
    let hookline = strict().await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap().to_string();

    for url in [
        "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
        "https://169.254.169.254/computeMetadata/v1/",
        "http://[fd00:ec2::254]/latest/meta-data/",
    ] {
        let (status, body) = hookline
            .post(
                &format!("/v1/apps/{}/endpoints", app),
                serde_json::json!({ "url": url }),
            )
            .await;
        assert_eq!(status, 400, "{} was accepted: {:?}", url, body);
    }
}

#[tokio::test]
async fn loopback_and_private_ranges_are_refused_however_they_are_spelled() {
    let hookline = strict().await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap().to_string();

    for url in [
        "https://127.0.0.1/hook",
        "https://127.1/hook",
        "https://[::1]/hook",
        // The same loopback address written as IPv4-mapped IPv6, which is the
        // spelling that gets past a check that only knows about dotted quads.
        "https://[::ffff:127.0.0.1]/hook",
        "https://10.0.0.5/hook",
        "https://192.168.1.1/hook",
        "https://172.16.0.1/hook",
        "https://[fc00::1]/hook",
        "https://[fe80::1]/hook",
        // Carrier-grade NAT and the benchmarking range, both of which reach
        // infrastructure rather than the public internet.
        "https://100.64.0.1/hook",
        "https://198.18.0.1/hook",
        "https://0.0.0.0/hook",
    ] {
        let (status, body) = hookline
            .post(
                &format!("/v1/apps/{}/endpoints", app),
                serde_json::json!({ "url": url }),
            )
            .await;
        assert_eq!(status, 400, "{} was accepted: {:?}", url, body);
    }
}

#[tokio::test]
async fn plaintext_and_credentials_in_the_url_are_refused() {
    let hookline = strict().await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap().to_string();

    for url in [
        "http://example.com/hook",
        "https://user:password@example.com/hook",
        "ftp://example.com/hook",
        "file:///etc/passwd",
        "gopher://example.com:70/",
        "not a url at all",
    ] {
        let (status, body) = hookline
            .post(
                &format!("/v1/apps/{}/endpoints", app),
                serde_json::json!({ "url": url }),
            )
            .await;
        assert_eq!(status, 400, "{} was accepted: {:?}", url, body);
    }
}

#[tokio::test]
async fn a_destination_that_becomes_private_is_refused_at_send_time() {
    // An endpoint created while the policy allowed loopback, delivered under a
    // policy that does not. The check that matters is the one at the moment of
    // sending: a policy tightened after the fact has to apply to what already
    // exists.
    let consumer = Consumer::start().await;
    let url = consumer.url();
    let hookline = Harness::start_with(|c| Config {
        destinations: hookline::guard::Policy {
            allow_http: true,
            ..Default::default()
        },
        retry: hookline::backoff::Schedule {
            max_attempts: 1,
            ..c.retry
        },
        ..c
    })
    .await;

    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap().to_string();
    // Insert the endpoint directly, as if it had been created under a looser
    // policy that has since been tightened.
    let endpoint_url = url.clone();
    let app_for_db = app.clone();
    let endpoint_id = hookline
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let (endpoint, _) = hookline::store::endpoints::create(
                &tx,
                &app_for_db,
                &endpoint_url,
                "",
                None,
                None,
                &hookline::sign::new_secret(),
                hookline::now_millis(),
            )?;
            tx.commit()?;
            Ok(endpoint.id)
        })
        .await
        .expect("insert the endpoint");

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "x", "payload": {} }),
        )
        .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (_, page) = hookline
            .get(&format!(
                "/v1/apps/{}/endpoints/{}/attempts",
                app, endpoint_id
            ))
            .await;
        if let Some(rows) = page["data"].as_array() {
            if !rows.is_empty() {
                let error = rows[0]["error"].as_str().unwrap_or("");
                assert!(
                    error.contains("refused"),
                    "the attempt should say it was refused, not {:?}",
                    error
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no attempt was recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(consumer.count(), 0, "the request must not have been made");
}

#[tokio::test]
async fn a_denied_host_is_refused_even_when_it_is_public() {
    let hookline = Harness::start_with(|c| Config {
        destinations: hookline::guard::Policy {
            denied_hosts: vec!["example.com".into()],
            ..Default::default()
        },
        ..c
    })
    .await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap().to_string();

    for url in ["https://example.com/hook", "https://sub.example.com/hook"] {
        let (status, body) = hookline
            .post(
                &format!("/v1/apps/{}/endpoints", app),
                serde_json::json!({ "url": url }),
            )
            .await;
        assert_eq!(status, 400, "{} was accepted: {:?}", url, body);
    }
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    // The last SSRF hole a URL check cannot close: a destination that passes
    // every test and then answers 307 to somewhere that would not.
    let consumer = Consumer::start().await;
    consumer.behave(support::Behaviour::Status(307));
    let hookline = Harness::start_with(|mut c| {
        c.retry.max_attempts = 1;
        c
    })
    .await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "x", "payload": {} }),
        )
        .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (_, page) = hookline
            .get(&format!("/v1/apps/{}/endpoints/{}/attempts", app, endpoint))
            .await;
        if let Some(rows) = page["data"].as_array() {
            if !rows.is_empty() {
                // Recorded as the redirect it is, not followed and not
                // mistaken for a success.
                assert_eq!(rows[0]["status"], "failure");
                assert_eq!(rows[0]["status_code"], 307);
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no attempt was recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        consumer.count(),
        1,
        "exactly one request, not a chain of them"
    );
}

#[tokio::test]
async fn an_oversized_payload_is_refused_at_the_door() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start_with(|mut c| {
        c.max_payload_bytes = 1024;
        c
    })
    .await;
    let (app, _, _) = hookline.wire(&consumer.url(), None).await;

    let big = "x".repeat(4096);
    let (status, body) = hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "x", "payload": { "blob": big } }),
        )
        .await;
    assert_eq!(status, 400, "{:?}", body);
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(consumer.count(), 0);
}
