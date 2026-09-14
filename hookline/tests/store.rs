//! What the storage layer promises, tested against a real database.
//!
//! These are the invariants the rest of the service is built on: an
//! application cannot see another's rows, a message with an idempotency key
//! fans out once however many times it is posted, and a rotation leaves both
//! secrets signing.

use hookline::db::Db;
use hookline::models::DeliveryStatus;
use hookline::sign;
use hookline::store;

const NOW: i64 = 1_700_000_000_000;

#[tokio::test]
async fn a_message_reaches_only_the_endpoints_that_want_it() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;

        let tx = conn.transaction()?;
        let (all, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        let (invoices, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://b.example/hook",
            "",
            Some(&["invoice.*".to_string()]),
            None,
            &sign::new_secret(),
            NOW,
        )?;
        let (paid_only, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://c.example/hook",
            "",
            Some(&["payment.paid".to_string()]),
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;

        let tx = conn.transaction()?;
        let (_, deliveries, replayed) = store::messages::create(
            &tx,
            &app.id,
            "invoice.paid",
            &serde_json::json!({"amount": 10}),
            None,
            NOW,
        )?;
        tx.commit()?;

        assert!(!replayed);
        let mut reached: Vec<String> = deliveries.iter().map(|d| d.endpoint_id.clone()).collect();
        reached.sort();
        let mut expected = vec![all.id, invoices.id];
        expected.sort();
        assert_eq!(reached, expected, "the wrong endpoints were queued");
        assert!(!deliveries.iter().any(|d| d.endpoint_id == paid_only.id));
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn an_idempotency_key_fans_out_once() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;

        let mut ids = Vec::new();
        for i in 0..3 {
            let tx = conn.transaction()?;
            let (message, deliveries, replayed) = store::messages::create(
                &tx,
                &app.id,
                "invoice.paid",
                &serde_json::json!({"attempt": i}),
                Some("key-1"),
                NOW + i,
            )?;
            tx.commit()?;
            assert_eq!(replayed, i > 0, "only the first post creates the message");
            assert_eq!(
                deliveries.len(),
                1,
                "a repeat must not queue a second delivery"
            );
            ids.push(message.id);
        }
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);

        let total: i64 = conn.query_row("SELECT count(*) FROM deliveries", [], |r| r.get(0))?;
        assert_eq!(total, 1);
        // The payload of the first post wins; a retry is the same request,
        // and treating a different body as an update would let a client
        // silently rewrite history.
        let stored = store::messages::get(conn, &app.id, &ids[0])?;
        assert_eq!(stored.payload["attempt"], 0);
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn two_applications_cannot_see_each_other() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let a = store::apps::create(conn, "a", None, &serde_json::json!({}), NOW)?;
        let b = store::apps::create(conn, "b", None, &serde_json::json!({}), NOW)?;

        let tx = conn.transaction()?;
        let (ep, _) = store::endpoints::create(
            &tx,
            &a.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        let tx = conn.transaction()?;
        let (_, deliveries, _) =
            store::messages::create(&tx, &a.id, "x", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;

        assert!(store::endpoints::get(conn, &b.id, &ep.id).is_err());
        assert!(store::deliveries::get(conn, &b.id, &deliveries[0].id).is_err());
        assert!(store::endpoints::list(conn, &b.id, None, 50)?
            .data
            .is_empty());
        assert!(store::messages::list(conn, &b.id, None, None, 50)?
            .data
            .is_empty());
        // And the owner still can.
        assert!(store::endpoints::get(conn, &a.id, &ep.id).is_ok());
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn a_rotation_keeps_both_secrets_signing_then_drops_the_old_one() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let first = sign::new_secret();
        let tx = conn.transaction()?;
        let (ep, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &first,
            NOW,
        )?;
        tx.commit()?;

        let grace = 24 * 60 * 60 * 1000;
        let second = sign::new_secret();
        let tx = conn.transaction()?;
        store::secrets::rotate(&tx, &ep.id, &second, grace, NOW)?;
        tx.commit()?;

        let during = store::secrets::active(conn, &ep.id, NOW + 1)?;
        assert_eq!(
            during.len(),
            2,
            "both secrets must sign during the grace period"
        );
        assert!(during.contains(&first) && during.contains(&second));

        let after = store::secrets::active(conn, &ep.id, NOW + grace + 1)?;
        assert_eq!(after, vec![second], "the old secret must stop signing");
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn an_endpoint_cannot_be_left_without_a_secret() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        let (ep, only) = store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;

        let refused = store::secrets::revoke(conn, &ep.id, &only.id, NOW);
        assert!(
            refused.is_err(),
            "revoking the last secret would break every future request"
        );
        assert_eq!(store::secrets::active(conn, &ep.id, NOW)?.len(), 1);

        let extra = store::secrets::add(conn, &ep.id, &sign::new_secret(), NOW)?;
        store::secrets::revoke(conn, &ep.id, &only.id, NOW)?;
        assert_eq!(
            store::secrets::active(conn, &ep.id, NOW)?,
            vec![extra.secret]
        );
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn disabling_an_endpoint_cancels_what_was_queued_for_it() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        let (ep, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        let tx = conn.transaction()?;
        let (_, deliveries, _) =
            store::messages::create(&tx, &app.id, "x", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;

        let tx = conn.transaction()?;
        let disabled = store::endpoints::disable(&tx, &app.id, &ep.id, "too many failures", NOW)?;
        tx.commit()?;
        assert!(disabled.is_disabled());

        let delivery = store::deliveries::get(conn, &app.id, &deliveries[0].id)?;
        assert_eq!(delivery.status, DeliveryStatus::Cancelled);

        // And a disabled endpoint receives nothing new.
        let tx = conn.transaction()?;
        let (_, queued, _) =
            store::messages::create(&tx, &app.id, "x", &serde_json::json!({}), None, NOW + 1)?;
        tx.commit()?;
        assert!(queued.is_empty());
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn a_replay_starts_the_retry_schedule_over() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        store::endpoints::create(
            &tx, &app.id, "https://a.example/hook", "", None, None, &sign::new_secret(), NOW,
        )?;
        tx.commit()?;
        let tx = conn.transaction()?;
        let (_, deliveries, _) =
            store::messages::create(&tx, &app.id, "x", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;
        let id = deliveries[0].id.clone();

        conn.execute(
            "UPDATE deliveries SET status = 'failed', attempts = 9, last_error = 'gone' WHERE id = ?1",
            [&id],
        )?;
        let replayed = store::deliveries::replay(conn, &app.id, &id, NOW + 5_000)?;
        assert_eq!(replayed.status, DeliveryStatus::Pending);
        assert_eq!(replayed.attempts, 0);
        assert_eq!(replayed.next_at, NOW + 5_000);
        assert!(replayed.last_error.is_none());
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn paging_walks_every_row_exactly_once() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        for i in 0..25 {
            let tx = conn.transaction()?;
            store::messages::create(
                &tx,
                &app.id,
                "x",
                &serde_json::json!({"i": i}),
                None,
                NOW + i,
            )?;
            tx.commit()?;
        }

        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store::messages::list(conn, &app.id, None, cursor.as_deref(), 7)?;
            seen.extend(page.data.iter().map(|m| m.id.clone()));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen.len(), 25);
        let unique: std::collections::HashSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), 25, "a row was returned on two pages");
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn an_application_can_be_addressed_by_the_uid_you_gave_it() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(
            conn,
            "acme",
            Some("customer-42"),
            &serde_json::json!({"plan": "pro"}),
            NOW,
        )?;
        assert_eq!(store::apps::get(conn, "customer-42")?.id, app.id);
        assert_eq!(
            store::apps::get(conn, &app.id)?.uid.as_deref(),
            Some("customer-42")
        );

        let clash = store::apps::create(
            conn,
            "other",
            Some("customer-42"),
            &serde_json::json!({}),
            NOW,
        );
        assert!(matches!(clash, Err(hookline::error::Error::Conflict(_))));
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn deleting_an_application_takes_everything_with_it() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        let tx = conn.transaction()?;
        store::messages::create(&tx, &app.id, "x", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;

        store::apps::delete(conn, &app.id)?;
        for table in ["endpoints", "endpoint_secrets", "messages", "deliveries"] {
            let left: i64 =
                conn.query_row(&format!("SELECT count(*) FROM {}", table), [], |r| r.get(0))?;
            assert_eq!(left, 0, "{} survived the cascade", table);
        }
        Ok(())
    })
    .await
    .expect("store");
}

#[tokio::test]
async fn the_secret_a_rotation_just_added_cannot_be_revoked() {
    // Right after a rotation the old secret is still active and the new one is
    // the only permanent one. A guard that asks "is anything active right now"
    // happily revokes the new one, and the endpoint has nothing to sign with
    // the instant the grace period lapses.
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        let (ep, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://a.example/hook",
            "",
            None,
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;

        let grace = 24 * 60 * 60 * 1000;
        let tx = conn.transaction()?;
        let fresh = store::secrets::rotate(&tx, &ep.id, &sign::new_secret(), grace, NOW)?;
        tx.commit()?;

        let refused = store::secrets::revoke(conn, &ep.id, &fresh.id, NOW);
        assert!(
            refused.is_err(),
            "revoking the only permanent secret leaves the endpoint unable to sign"
        );
        assert!(
            !store::secrets::active(conn, &ep.id, NOW + grace + 1)?.is_empty(),
            "there must still be a secret once the grace period has passed"
        );

        // The expiring one may be revoked: a permanent secret remains.
        let secrets = store::secrets::list(conn, &ep.id)?;
        let expiring = secrets
            .iter()
            .find(|s| s.expires_at.is_some())
            .expect("the old secret");
        store::secrets::revoke(conn, &ep.id, &expiring.id, NOW)?;
        assert_eq!(
            store::secrets::active(conn, &ep.id, NOW + grace + 1)?.len(),
            1
        );
        Ok(())
    })
    .await
    .expect("store");
}
