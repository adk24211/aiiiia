//! What a customer is buying, end to end.
//!
//! Every test here runs the real server against a real consumer over real
//! HTTP: post a message, watch it arrive, check the signature the way the
//! consumer's own library would.

mod support;

use hookline::sign;
use std::time::Duration;
use support::{Behaviour, Consumer, Harness};

const PATIENCE: Duration = Duration::from_secs(10);

#[tokio::test]
async fn a_message_arrives_signed() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, _, secret) = hookline.wire(&consumer.url(), None).await;

    let (status, sent) = hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({
                "event_type": "invoice.paid",
                "payload": { "amount": 4200, "currency": "usd" }
            }),
        )
        .await;
    assert_eq!(status, 202, "{:?}", sent);
    assert_eq!(sent["deliveries"].as_array().expect("deliveries").len(), 1);

    let got = consumer.wait_for(1, PATIENCE).await;
    let request = &got[0];

    // The three headers a consumer needs, and the signature over exactly the
    // bytes that were sent.
    let id = request.header("webhook-id").expect("webhook-id");
    let timestamp: i64 = request
        .header("webhook-timestamp")
        .expect("webhook-timestamp")
        .parse()
        .expect("a unix timestamp");
    let signature = request
        .header("webhook-signature")
        .expect("webhook-signature");
    assert_eq!(id, sent["id"].as_str().unwrap());
    assert_eq!(request.header("content-type"), Some("application/json"));

    sign::verify(
        &secret,
        signature,
        id,
        timestamp,
        request.body.as_bytes(),
        timestamp,
        sign::DEFAULT_TOLERANCE_SECS,
    )
    .expect("the signature a consumer checks must verify");

    // And the delivery is recorded as done.
    let (_, deliveries) = hookline
        .get(&format!("/v1/apps/{}/messages/{}/deliveries", app, id))
        .await;
    assert_eq!(deliveries[0]["status"], "succeeded");
    assert_eq!(deliveries[0]["attempts"], 1);
}

#[tokio::test]
async fn a_flapping_endpoint_gets_the_message_in_the_end() {
    let consumer = Consumer::start().await;
    consumer.behave(Behaviour::FailThen(2));
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": { "n": 1 } }),
        )
        .await;

    let got = consumer.wait_for(3, PATIENCE).await;
    assert_eq!(got.len(), 3, "two failures then a success");
    // Each attempt says which it is, so a consumer's own log can tell a retry
    // from a first delivery.
    assert_eq!(got[0].header("hookline-attempt"), Some("1"));
    assert_eq!(got[2].header("hookline-attempt"), Some("3"));
    // The same message id throughout, which is what makes consumer-side
    // deduplication possible at all.
    assert_eq!(got[0].header("webhook-id"), got[2].header("webhook-id"));

    let deliveries = wait_for_status(&hookline, &app, &endpoint, "succeeded").await;
    assert_eq!(deliveries[0]["attempts"], 3);

    let id = deliveries[0]["id"].as_str().unwrap();
    let (_, attempts) = hookline
        .get(&format!("/v1/apps/{}/deliveries/{}/attempts", app, id))
        .await;
    let attempts = attempts.as_array().expect("attempts");
    assert_eq!(attempts.len(), 3, "every request is in the audit trail");
    assert_eq!(attempts[0]["status"], "failure");
    assert_eq!(attempts[0]["status_code"], 500);
    assert_eq!(attempts[2]["status"], "success");
}

#[tokio::test]
async fn a_four_hundred_is_not_retried() {
    let consumer = Consumer::start().await;
    consumer.behave(Behaviour::Status(422));
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": {} }),
        )
        .await;

    let deliveries = wait_for_status(&hookline, &app, &endpoint, "failed").await;
    assert_eq!(
        deliveries[0]["attempts"], 1,
        "an endpoint that says the request is wrong should not be asked nine more times"
    );
    assert_eq!(consumer.count(), 1);
}

#[tokio::test]
async fn a_timeout_is_retried_and_recorded() {
    let consumer = Consumer::start().await;
    consumer.behave(Behaviour::Hang);
    let hookline = Harness::start_with(|mut c| {
        c.request_timeout = Duration::from_millis(300);
        c.lease = Duration::from_secs(5);
        c.retry.max_attempts = 2;
        c
    })
    .await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": {} }),
        )
        .await;

    let deliveries = wait_for_status(&hookline, &app, &endpoint, "failed").await;
    let id = deliveries[0]["id"].as_str().unwrap();
    let (_, attempts) = hookline
        .get(&format!("/v1/apps/{}/deliveries/{}/attempts", app, id))
        .await;
    let attempts = attempts.as_array().expect("attempts");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["error"], "the request timed out");
    assert!(attempts[0]["status_code"].is_null());
}

#[tokio::test]
async fn only_the_endpoints_that_subscribed_are_called() {
    let wanted = Consumer::start().await;
    let unwanted = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, _, _) = hookline.wire(&wanted.url(), Some(vec!["invoice.*"])).await;
    hookline
        .post(
            &format!("/v1/apps/{}/endpoints", app),
            serde_json::json!({ "url": unwanted.url(), "event_types": ["user.created"] }),
        )
        .await;

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": {} }),
        )
        .await;

    wanted.wait_for(1, PATIENCE).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        unwanted.count(),
        0,
        "an endpoint got an event it did not subscribe to"
    );
}

#[tokio::test]
async fn a_rotation_is_not_an_outage() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, endpoint, old) = hookline.wire(&consumer.url(), None).await;

    let (status, rotated) = hookline
        .post(
            &format!("/v1/apps/{}/endpoints/{}/secrets/rotate", app, endpoint),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, 200, "{:?}", rotated);
    let new = rotated["secret"].as_str().expect("a secret").to_string();
    assert_ne!(new, old);

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": { "x": 1 } }),
        )
        .await;
    let got = consumer.wait_for(1, PATIENCE).await;
    let request = &got[0];
    let id = request.header("webhook-id").unwrap();
    let timestamp: i64 = request
        .header("webhook-timestamp")
        .unwrap()
        .parse()
        .unwrap();
    let header = request.header("webhook-signature").unwrap();

    // A consumer that has not yet picked up the new secret must still verify,
    // and one that has must verify too. That is the whole point of a rotation
    // window: neither side has to change at the same moment.
    for secret in [&old, &new] {
        sign::verify(
            secret,
            header,
            id,
            timestamp,
            request.body.as_bytes(),
            timestamp,
            sign::DEFAULT_TOLERANCE_SECS,
        )
        .unwrap_or_else(|e| {
            panic!(
                "{:?} should verify during the rotation window: {:?}",
                secret, e
            )
        });
    }
}

#[tokio::test]
async fn a_replay_sends_it_again() {
    let consumer = Consumer::start().await;
    consumer.behave(Behaviour::Status(410));
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": { "x": 1 } }),
        )
        .await;
    let deliveries = wait_for_status(&hookline, &app, &endpoint, "failed").await;
    let id = deliveries[0]["id"].as_str().unwrap().to_string();

    consumer.behave(Behaviour::Ok);
    let (status, replayed) = hookline
        .post(
            &format!("/v1/apps/{}/deliveries/{}/replay", app, id),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, 200, "{:?}", replayed);
    assert_eq!(
        replayed["attempts"], 0,
        "a replay gets the full schedule again"
    );

    consumer.wait_for(2, PATIENCE).await;
    let deliveries = wait_for_status(&hookline, &app, &endpoint, "succeeded").await;
    assert_eq!(deliveries[0]["id"], id, "the same delivery, not a new one");
}

#[tokio::test]
async fn a_bulk_replay_brings_an_endpoint_back_up_to_date() {
    let consumer = Consumer::start().await;
    consumer.behave(Behaviour::Status(404));
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    for i in 0..5 {
        hookline
            .post(
                &format!("/v1/apps/{}/messages", app),
                serde_json::json!({ "event_type": "invoice.paid", "payload": { "i": i } }),
            )
            .await;
    }
    wait_until(PATIENCE, || async {
        let (_, page) = hookline
            .get(&format!(
                "/v1/apps/{}/endpoints/{}/deliveries?status=failed",
                app, endpoint
            ))
            .await;
        page["data"].as_array().map(|d| d.len()).unwrap_or(0) == 5
    })
    .await;

    consumer.behave(Behaviour::Ok);
    let (status, result) = hookline
        .post(
            &format!("/v1/apps/{}/endpoints/{}/replay", app, endpoint),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, 200, "{:?}", result);
    assert_eq!(result["replayed"], 5);
    assert_eq!(result["more"], false);

    consumer.wait_for(10, PATIENCE).await;
    wait_until(PATIENCE, || async {
        let (_, page) = hookline
            .get(&format!(
                "/v1/apps/{}/endpoints/{}/deliveries?status=succeeded",
                app, endpoint
            ))
            .await;
        page["data"].as_array().map(|d| d.len()).unwrap_or(0) == 5
    })
    .await;
}

#[tokio::test]
async fn a_disabled_endpoint_stops_receiving_and_starts_again() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    hookline
        .post(
            &format!("/v1/apps/{}/endpoints/{}/disable", app, endpoint),
            serde_json::json!({ "reason": "the customer asked" }),
        )
        .await;
    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": {} }),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(consumer.count(), 0);

    hookline
        .post(
            &format!("/v1/apps/{}/endpoints/{}/enable", app, endpoint),
            serde_json::json!({}),
        )
        .await;
    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "invoice.paid", "payload": {} }),
        )
        .await;
    consumer.wait_for(1, PATIENCE).await;
}

#[tokio::test]
async fn a_repeated_idempotency_key_is_delivered_once() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, _, _) = hookline.wire(&consumer.url(), None).await;

    let body = serde_json::json!({
        "event_type": "invoice.paid",
        "payload": { "amount": 100 },
        "idempotency_key": "charge-42"
    });
    let (first_status, first) = hookline
        .post(&format!("/v1/apps/{}/messages", app), body.clone())
        .await;
    let (second_status, second) = hookline
        .post(&format!("/v1/apps/{}/messages", app), body)
        .await;

    assert_eq!(first_status, 202);
    assert_eq!(second_status, 200, "a repeat is not a new message");
    assert_eq!(first["id"], second["id"]);
    assert_eq!(second["duplicate"], true);

    consumer.wait_for(1, PATIENCE).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        consumer.count(),
        1,
        "the consumer must not receive it twice"
    );
}

#[tokio::test]
async fn the_payload_is_delivered_byte_for_byte() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, _, secret) = hookline.wire(&consumer.url(), None).await;

    // Unicode, an empty string, a null, a float and a nested array: a payload
    // that round-trips through anything careless will come out different, and
    // a signature over different bytes does not verify.
    let payload = serde_json::json!({
        "name": "주식회사 아크메",
        "emoji_free": "—",
        "empty": "",
        "missing": null,
        "ratio": 0.1,
        "items": [1, [2, [3]]],
        "escaped": "a\"b\\c\nd"
    });
    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "x", "payload": payload }),
        )
        .await;

    let got = consumer.wait_for(1, PATIENCE).await;
    let request = &got[0];
    let delivered: serde_json::Value =
        serde_json::from_str(&request.body).expect("the body should be JSON");
    assert_eq!(delivered, payload);

    sign::verify(
        &secret,
        request.header("webhook-signature").unwrap(),
        request.header("webhook-id").unwrap(),
        request
            .header("webhook-timestamp")
            .unwrap()
            .parse()
            .unwrap(),
        request.body.as_bytes(),
        request
            .header("webhook-timestamp")
            .unwrap()
            .parse()
            .unwrap(),
        sign::DEFAULT_TOLERANCE_SECS,
    )
    .expect("the signature must cover the delivered bytes");
}

/// Poll a condition until it holds, or fail the test.
async fn wait_until<F, Fut>(within: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + within;
    loop {
        if check().await {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("the condition did not hold within {:?}", within);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_status(
    hookline: &Harness,
    app: &str,
    endpoint: &str,
    status: &str,
) -> Vec<serde_json::Value> {
    let path = format!(
        "/v1/apps/{}/endpoints/{}/deliveries?status={}",
        app, endpoint, status
    );
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let (_, page) = hookline.get(&path).await;
        if let Some(rows) = page["data"].as_array() {
            if !rows.is_empty() {
                return rows.clone();
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!("no delivery reached {:?} within {:?}", status, PATIENCE);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_rate_limited_endpoint_is_fed_slowly_and_still_fed() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap().to_string();
    // 600 a minute is one every 100ms, which is slow enough to observe and
    // fast enough to finish inside a test.
    hookline
        .post(
            &format!("/v1/apps/{}/endpoints", app),
            serde_json::json!({ "url": consumer.url(), "rate_limit": 600 }),
        )
        .await;

    for i in 0..6 {
        hookline
            .post(
                &format!("/v1/apps/{}/messages", app),
                serde_json::json!({ "event_type": "x", "payload": { "i": i } }),
            )
            .await;
    }

    // Without a limit all six would be in flight within a poll interval.
    consumer.wait_for(1, PATIENCE).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        consumer.count() < 6,
        "the limit did not slow anything down: {} arrived at once",
        consumer.count()
    );

    // And it is a limit, not a cap: everything arrives in the end.
    consumer.wait_for(6, PATIENCE).await;
}
