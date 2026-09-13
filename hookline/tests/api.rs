//! The API's own promises: what a caller is allowed to do, what they are told
//! when they may not, and what a scope actually restricts.

mod support;

use support::{Consumer, Harness};

#[tokio::test]
async fn every_route_needs_a_credential() {
    let hookline = Harness::start().await;
    for path in ["/v1/apps", "/v1/stats", "/v1/keys"] {
        let (status, body) = hookline.get_as("", path).await;
        assert_eq!(status, 401, "{} answered without a token: {:?}", path, body);
        let (status, _) = hookline.get_as("hl_not-a-real-token", path).await;
        assert_eq!(status, 401, "{} accepted a made-up token", path);
    }
    // The health check is the exception, on purpose: one that needs a
    // credential is one a load balancer cannot make.
    let response = hookline
        .client
        .get(format!("{}/health", hookline.base))
        .send()
        .await
        .expect("health");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_revoked_key_stops_working_immediately() {
    let hookline = Harness::start().await;
    let (_, created) = hookline
        .post(
            "/v1/keys",
            serde_json::json!({ "name": "temporary", "scope": "read" }),
        )
        .await;
    let token = created["token"].as_str().expect("a token").to_string();
    let id = created["id"].as_str().expect("an id").to_string();

    let (status, _) = hookline.get_as(&token, "/v1/apps").await;
    assert_eq!(status, 200);

    hookline.delete(&format!("/v1/keys/{}", id)).await;
    let (status, _) = hookline.get_as(&token, "/v1/apps").await;
    assert_eq!(status, 401, "a revoked key should stop working at once");
}

#[tokio::test]
async fn a_publishing_key_can_only_publish() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, _, _) = hookline.wire(&consumer.url(), None).await;
    let (_, created) = hookline
        .post("/v1/keys", serde_json::json!({ "name": "app-server" }))
        .await;
    let token = created["token"].as_str().expect("a token").to_string();
    assert_eq!(
        created["scope"], "publish",
        "publish is the default for a good reason"
    );

    let (status, body) = hookline
        .post_as(
            &token,
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "x", "payload": {} }),
        )
        .await;
    assert_eq!(status, 202, "{:?}", body);

    // It cannot read history, rotate a secret, or mint another key.
    for path in [
        format!("/v1/apps/{}/endpoints", app),
        format!("/v1/apps/{}/messages", app),
        "/v1/keys".to_string(),
        "/v1/stats".to_string(),
    ] {
        let (status, body) = hookline.get_as(&token, &path).await;
        assert_eq!(status, 403, "{} was readable: {:?}", path, body);
        assert_eq!(body["error"]["code"], "forbidden");
    }
}

#[tokio::test]
async fn one_application_cannot_reach_anothers_data() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (a, endpoint, _) = hookline.wire(&consumer.url(), None).await;
    let (_, other) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "other" }))
        .await;
    let b = other["id"].as_str().unwrap();

    let (status, body) = hookline
        .get(&format!("/v1/apps/{}/endpoints/{}", b, endpoint))
        .await;
    assert_eq!(status, 404, "{:?}", body);
    assert_eq!(body["error"]["code"], "not_found");

    let (_, mine) = hookline
        .get(&format!("/v1/apps/{}/endpoints/{}", a, endpoint))
        .await;
    assert_eq!(mine["id"], endpoint);
}

#[tokio::test]
async fn an_application_answers_to_the_uid_you_gave_it() {
    let hookline = Harness::start().await;
    let (_, app) = hookline
        .post(
            "/v1/apps",
            serde_json::json!({ "name": "acme", "uid": "customer-42", "metadata": { "plan": "pro" } }),
        )
        .await;
    let id = app["id"].as_str().unwrap();

    let (status, by_uid) = hookline.get("/v1/apps/customer-42").await;
    assert_eq!(status, 200);
    assert_eq!(by_uid["id"], id);
    assert_eq!(by_uid["metadata"]["plan"], "pro");

    let (status, clash) = hookline
        .post(
            "/v1/apps",
            serde_json::json!({ "name": "other", "uid": "customer-42" }),
        )
        .await;
    assert_eq!(status, 409, "{:?}", clash);
    assert_eq!(clash["error"]["code"], "conflict");
}

#[tokio::test]
async fn deleting_an_application_wants_its_name_back() {
    let hookline = Harness::start().await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let id = app["id"].as_str().unwrap().to_string();

    let (status, body) = hookline.delete(&format!("/v1/apps/{}", id)).await;
    assert_eq!(
        status, 400,
        "a bare delete should not destroy history: {:?}",
        body
    );
    let (status, _) = hookline.get(&format!("/v1/apps/{}", id)).await;
    assert_eq!(status, 200, "and the application should still be there");

    let (status, _) = hookline
        .delete(&format!("/v1/apps/{}?confirm_name=acme", id))
        .await;
    assert_eq!(status, 200);
    let (status, _) = hookline.get(&format!("/v1/apps/{}", id)).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn clearing_a_filter_and_not_mentioning_it_are_different() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline
        .wire(&consumer.url(), Some(vec!["invoice.*"]))
        .await;
    let path = format!("/v1/apps/{}/endpoints/{}", app, endpoint);

    // A patch that does not mention the filter leaves it alone.
    let (_, patched) = hookline
        .patch(
            &path,
            serde_json::json!({ "description": "the billing hook" }),
        )
        .await;
    assert_eq!(patched["event_types"], serde_json::json!(["invoice.*"]));
    assert_eq!(patched["description"], "the billing hook");

    // A patch that sets it to null clears it.
    let (_, patched) = hookline
        .patch(&path, serde_json::json!({ "event_types": null }))
        .await;
    assert!(patched["event_types"].is_null(), "{:?}", patched);

    hookline
        .post(
            &format!("/v1/apps/{}/messages", app),
            serde_json::json!({ "event_type": "user.created", "payload": {} }),
        )
        .await;
    consumer
        .wait_for(1, std::time::Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn a_wildcard_is_only_allowed_at_the_end() {
    let hookline = Harness::start().await;
    let (_, app) = hookline
        .post("/v1/apps", serde_json::json!({ "name": "acme" }))
        .await;
    let app = app["id"].as_str().unwrap();

    let (status, body) = hookline
        .post(
            &format!("/v1/apps/{}/endpoints", app),
            serde_json::json!({ "url": "https://example.com/hook", "event_types": ["*.paid"] }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a leading star reads as a wildcard and is not one: {:?}",
        body
    );
}

#[tokio::test]
async fn an_endpoint_cannot_be_left_unable_to_sign() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;
    let (_, secrets) = hookline
        .get(&format!("/v1/apps/{}/endpoints/{}/secrets", app, endpoint))
        .await;
    let only = secrets[0]["id"].as_str().unwrap().to_string();

    let (status, body) = hookline
        .delete(&format!(
            "/v1/apps/{}/endpoints/{}/secrets/{}",
            app, endpoint, only
        ))
        .await;
    assert_eq!(status, 409, "{:?}", body);
    assert_eq!(body["error"]["code"], "conflict");
}

#[tokio::test]
async fn paging_is_stable_while_rows_are_inserted() {
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, _, _) = hookline.wire(&consumer.url(), None).await;

    for i in 0..12 {
        hookline
            .post(
                &format!("/v1/apps/{}/messages", app),
                serde_json::json!({ "event_type": "x", "payload": { "i": i } }),
            )
            .await;
    }

    let (_, first) = hookline
        .get(&format!("/v1/apps/{}/messages?limit=5", app))
        .await;
    assert_eq!(first["data"].as_array().unwrap().len(), 5);
    assert_eq!(first["has_more"], true);
    let cursor = first["next_cursor"].as_str().expect("a cursor").to_string();

    // Insert more between the pages. A cursor is an id, not an offset, so the
    // second page is the rows after the first — not five rows shifted by
    // whatever arrived in between.
    for i in 100..105 {
        hookline
            .post(
                &format!("/v1/apps/{}/messages", app),
                serde_json::json!({ "event_type": "x", "payload": { "i": i } }),
            )
            .await;
    }

    let (_, second) = hookline
        .get(&format!(
            "/v1/apps/{}/messages?limit=5&cursor={}",
            app, cursor
        ))
        .await;
    let first_ids: Vec<&str> = first["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    for row in second["data"].as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        assert!(!first_ids.contains(&id), "{} appeared on both pages", id);
    }
}

#[tokio::test]
async fn an_unknown_route_is_a_json_404() {
    let hookline = Harness::start().await;
    let (status, body) = hookline.get("/v1/nope").await;
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn a_replay_clears_the_circuit_the_failures_opened() {
    // A breaker that stays open through an explicit replay is a replay that
    // reports what it queued and delivers nothing.
    let consumer = Consumer::start().await;
    consumer.behave(support::Behaviour::Status(500));
    let hookline = Harness::start_with(|mut c| {
        c.breaker.failures_to_open = 2;
        c.breaker.cooldown = std::time::Duration::from_secs(600);
        c.retry.max_attempts = 1;
        c
    })
    .await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;

    for i in 0..3 {
        hookline
            .post(
                &format!("/v1/apps/{}/messages", app),
                serde_json::json!({ "event_type": "x", "payload": { "i": i } }),
            )
            .await;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, health) = hookline
            .get(&format!("/v1/apps/{}/endpoints/{}/health", app, endpoint))
            .await;
        if !health["circuit_open_until"].is_null() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the circuit never opened"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    consumer.behave(support::Behaviour::Ok);
    let before = consumer.count();
    let (status, result) = hookline
        .post(
            &format!("/v1/apps/{}/endpoints/{}/replay", app, endpoint),
            serde_json::json!({}),
        )
        .await;
    assert_eq!(status, 200, "{:?}", result);
    assert!(result["replayed"].as_u64().unwrap() > 0);

    consumer
        .wait_for(before + 1, std::time::Duration::from_secs(10))
        .await;
    let (_, health) = hookline
        .get(&format!("/v1/apps/{}/endpoints/{}/health", app, endpoint))
        .await;
    assert!(health["circuit_open_until"].is_null());
}

#[tokio::test]
async fn a_limit_is_honoured_on_every_listing() {
    // Not a formality: a listing whose limit silently fails to parse either
    // returns everything or returns an error, and both are found in
    // production rather than here.
    let consumer = Consumer::start().await;
    let hookline = Harness::start().await;
    let (app, endpoint, _) = hookline.wire(&consumer.url(), None).await;
    for i in 0..4 {
        hookline
            .post(
                &format!("/v1/apps/{}/messages", app),
                serde_json::json!({ "event_type": "x", "payload": { "i": i } }),
            )
            .await;
    }
    consumer
        .wait_for(4, std::time::Duration::from_secs(10))
        .await;

    for path in [
        "/v1/apps?limit=2".to_string(),
        format!("/v1/apps/{}/endpoints?limit=2", app),
        format!("/v1/apps/{}/messages?limit=2", app),
        format!("/v1/apps/{}/endpoints/{}/deliveries?limit=2", app, endpoint),
        format!("/v1/apps/{}/endpoints/{}/attempts?limit=2", app, endpoint),
        format!("/v1/apps/{}/messages?limit=2&event_type=x", app),
        format!(
            "/v1/apps/{}/endpoints/{}/deliveries?limit=2&status=succeeded",
            app, endpoint
        ),
    ] {
        let (status, body) = hookline.get(&path).await;
        assert_eq!(status, 200, "{} answered {}: {:?}", path, status, body);
        let rows = body["data"]
            .as_array()
            .unwrap_or_else(|| panic!("{} returned {:?}", path, body));
        assert!(
            rows.len() <= 2,
            "{} ignored the limit and returned {}",
            path,
            rows.len()
        );
    }
}

#[tokio::test]
async fn the_admin_ui_is_served_and_locked_down() {
    let hookline = Harness::start().await;
    let response = hookline
        .client
        .get(&hookline.base)
        .send()
        .await
        .expect("the admin UI should be served");
    assert_eq!(response.status(), 200);
    let policy = response
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // It loads nothing from anywhere else and posts nowhere, so say so.
    assert!(policy.contains("default-src 'none'"), "{}", policy);
    assert!(policy.contains("connect-src 'self'"), "{}", policy);

    let body = response.text().await.expect("body");
    assert!(body.contains("<title>hookline</title>"));
    // One file, no build step, and nothing fetched from a CDN at runtime.
    assert!(
        !body.contains("http://"),
        "the UI must not load anything over plaintext"
    );
    assert!(
        !body.contains("<script src"),
        "the UI must not depend on a script it does not ship"
    );
}

#[tokio::test]
async fn the_admin_ui_can_be_switched_off() {
    let hookline = Harness::start_with(|mut c| {
        c.admin_ui = false;
        c
    })
    .await;
    let response = hookline
        .client
        .get(&hookline.base)
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 404);
}
