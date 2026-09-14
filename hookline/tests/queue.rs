//! The queue's promises: nothing is claimed twice, a dead worker's work comes
//! back, and an endpoint that is down cannot occupy every worker.

use hookline::backoff;
use hookline::breaker;
use hookline::db::Db;
use hookline::models::DeliveryStatus;
use hookline::queue::{self, Outcome, Settled};
use hookline::sign;
use hookline::store;
use rusqlite::Connection;

const NOW: i64 = 1_700_000_000_000;

/// One application, one endpoint, and `count` messages queued to it.
fn seed(conn: &mut Connection, count: usize) -> hookline::error::Result<(String, String)> {
    let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
    let tx = conn.transaction()?;
    let (endpoint, _) = store::endpoints::create(
        &tx,
        &app.id,
        "https://example.com/hook",
        "",
        None,
        None,
        &sign::new_secret(),
        NOW,
    )?;
    tx.commit()?;
    for i in 0..count {
        let tx = conn.transaction()?;
        store::messages::create(
            &tx,
            &app.id,
            "invoice.paid",
            &serde_json::json!({ "i": i }),
            None,
            NOW,
        )?;
        tx.commit()?;
    }
    Ok((app.id, endpoint.id))
}

fn failure(code: u16) -> Outcome {
    Outcome {
        succeeded: false,
        status_code: Some(code),
        error: None,
        duration_ms: 12,
        response_snippet: Some("upstream error".into()),
    }
}

fn success() -> Outcome {
    Outcome {
        succeeded: true,
        status_code: Some(200),
        error: None,
        duration_ms: 9,
        response_snippet: None,
    }
}

#[tokio::test]
async fn a_claim_takes_the_whole_batch_and_leaves_the_rest() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        seed(conn, 10)?;
        let tx = conn.transaction()?;
        let first = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 4)?;
        tx.commit()?;
        assert_eq!(first.len(), 4);

        let tx = conn.transaction()?;
        let second = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert_eq!(
            second.len(),
            6,
            "the leased four must not be handed out again"
        );

        let overlap: Vec<_> = second
            .iter()
            .filter(|j| first.iter().any(|f| f.delivery.id == j.delivery.id))
            .collect();
        assert!(overlap.is_empty(), "a delivery was claimed twice");

        let tx = conn.transaction()?;
        let third = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert!(third.is_empty(), "everything is leased");
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_dead_workers_lease_expires_and_the_work_comes_back() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        seed(conn, 3)?;
        let tx = conn.transaction()?;
        let claimed = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 3)?;
        tx.commit()?;
        assert_eq!(claimed.len(), 3);

        let tx = conn.transaction()?;
        let during = queue::claim(&tx, NOW + queue::DEFAULT_LEASE - 1, queue::DEFAULT_LEASE, 3)?;
        tx.commit()?;
        assert!(during.is_empty(), "a live lease must be respected");

        let tx = conn.transaction()?;
        let after = queue::claim(&tx, NOW + queue::DEFAULT_LEASE + 1, queue::DEFAULT_LEASE, 3)?;
        tx.commit()?;
        assert_eq!(after.len(), 3, "an expired lease must release the work");
        // No attempt was made, so no retry was spent.
        assert!(after.iter().all(|j| j.delivery.attempts == 0));
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_job_carries_what_it_needs_to_be_sent() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let (app_id, endpoint_id) = seed(conn, 1)?;
        let tx = conn.transaction()?;
        let jobs = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 1)?;
        tx.commit()?;

        let job = &jobs[0];
        assert_eq!(job.endpoint.id, endpoint_id);
        assert_eq!(job.delivery.app_id, app_id);
        assert_eq!(job.message.event_type, "invoice.paid");
        assert_eq!(job.secrets.len(), 1);
        assert!(job.secrets[0].starts_with("whsec_"));
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_success_ends_the_delivery_and_clears_the_health_record() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let (app_id, endpoint_id) = seed(conn, 1)?;
        let policy = breaker::Policy::default();

        let tx = conn.transaction()?;
        let job = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 1)?.remove(0);
        let settled = queue::settle(&tx, &job, &success(), Some(NOW + 5_000), &policy, NOW)?;
        tx.commit()?;

        assert_eq!(settled, Settled::Succeeded);
        let delivery = store::deliveries::get(conn, &app_id, &job.delivery.id)?;
        assert_eq!(delivery.status, DeliveryStatus::Succeeded);
        assert_eq!(delivery.attempts, 1);
        assert_eq!(
            store::health::get(conn, &endpoint_id)?.consecutive_failures,
            0
        );

        let attempts = store::attempts::for_delivery(conn, &app_id, &job.delivery.id)?;
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "success");
        assert_eq!(attempts[0].status_code, Some(200));
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_failure_schedules_a_retry_and_records_the_attempt() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let (app_id, _) = seed(conn, 1)?;
        let policy = breaker::Policy::default();

        let tx = conn.transaction()?;
        let job = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 1)?.remove(0);
        let settled = queue::settle(&tx, &job, &failure(503), Some(NOW + 5_000), &policy, NOW)?;
        tx.commit()?;

        assert_eq!(
            settled,
            Settled::Retrying {
                next_at: NOW + 5_000
            }
        );
        let delivery = store::deliveries::get(conn, &app_id, &job.delivery.id)?;
        assert_eq!(delivery.status, DeliveryStatus::Pending);
        assert_eq!(delivery.attempts, 1);
        assert_eq!(delivery.next_at, NOW + 5_000);
        assert_eq!(
            delivery.last_error.as_deref(),
            Some("the endpoint answered 503")
        );

        // Not due yet, so it is not handed out.
        let tx = conn.transaction()?;
        let early = queue::claim(&tx, NOW + 4_999, queue::DEFAULT_LEASE, 1)?;
        tx.commit()?;
        assert!(early.is_empty());

        let tx = conn.transaction()?;
        let due = queue::claim(&tx, NOW + 5_000, queue::DEFAULT_LEASE, 1)?;
        tx.commit()?;
        assert_eq!(due.len(), 1);
        assert_eq!(
            due[0].delivery.attempts, 1,
            "the retry knows which attempt it is"
        );
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn the_last_failure_ends_the_delivery() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let (app_id, _) = seed(conn, 1)?;
        let policy = breaker::Policy::default();

        let tx = conn.transaction()?;
        let job = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 1)?.remove(0);
        let settled = queue::settle(&tx, &job, &failure(500), None, &policy, NOW)?;
        tx.commit()?;

        assert_eq!(settled, Settled::Failed);
        let delivery = store::deliveries::get(conn, &app_id, &job.delivery.id)?;
        assert_eq!(delivery.status, DeliveryStatus::Failed);

        let tx = conn.transaction()?;
        let more = queue::claim(&tx, NOW + 1_000_000, queue::DEFAULT_LEASE, 10)?;
        tx.commit()?;
        assert!(more.is_empty(), "a failed delivery must not be retried");
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn an_open_circuit_keeps_a_dead_endpoint_from_taking_every_worker() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        seed(conn, 20)?;
        let policy = breaker::Policy::default();
        let schedule = backoff::Schedule::default();

        // Fail the threshold number of deliveries.
        let mut at = NOW;
        for _ in 0..policy.failures_to_open {
            let tx = conn.transaction()?;
            let job = queue::claim(&tx, at, queue::DEFAULT_LEASE, 1)?.remove(0);
            let retry_at = schedule.delay(1).map(|d| at + d.as_millis() as i64);
            queue::settle(&tx, &job, &failure(500), retry_at, &policy, at)?;
            tx.commit()?;
            at += 1;
        }

        let tx = conn.transaction()?;
        let blocked = queue::claim(&tx, at, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert!(
            blocked.is_empty(),
            "the circuit is open, nothing should be claimed"
        );

        // When the cooldown passes, work flows again.
        let cooldown = policy.cooldown_for(policy.failures_to_open).as_millis() as i64;
        let tx = conn.transaction()?;
        let after = queue::claim(&tx, at + cooldown + 1, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert!(
            !after.is_empty(),
            "the circuit should let work through once it decays"
        );
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_success_closes_the_circuit_again() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        seed(conn, 20)?;
        let policy = breaker::Policy::default();

        let mut at = NOW;
        for _ in 0..policy.failures_to_open {
            let tx = conn.transaction()?;
            let job = queue::claim(&tx, at, queue::DEFAULT_LEASE, 1)?.remove(0);
            queue::settle(&tx, &job, &failure(500), Some(at + 1), &policy, at)?;
            tx.commit()?;
            at += 1;
        }
        let cooldown = policy.cooldown_for(policy.failures_to_open).as_millis() as i64;
        at += cooldown + 1;

        let tx = conn.transaction()?;
        let job = queue::claim(&tx, at, queue::DEFAULT_LEASE, 1)?.remove(0);
        queue::settle(&tx, &job, &success(), Some(at + 1), &policy, at)?;
        tx.commit()?;

        let tx = conn.transaction()?;
        let flowing = queue::claim(&tx, at, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert!(!flowing.is_empty(), "one success should close the circuit");
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn an_endpoint_that_never_recovers_is_switched_off() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let (app_id, endpoint_id) = seed(conn, 1)?;
        // A policy that gives up immediately, so the test does not have to
        // spend two hundred attempts to reach the interesting line.
        let policy = breaker::Policy {
            failures_to_disable: Some(2),
            ..breaker::Policy::default()
        };

        let mut at = NOW;
        let mut last = Settled::Failed;
        for _ in 0..2 {
            let tx = conn.transaction()?;
            let Some(job) = queue::claim(&tx, at, queue::DEFAULT_LEASE, 1)?.pop() else {
                tx.commit()?;
                break;
            };
            last = queue::settle(&tx, &job, &failure(500), Some(at + 1), &policy, at)?;
            tx.commit()?;
            at += 2;
        }

        assert_eq!(last, Settled::FailedAndDisabled);
        let endpoint = store::endpoints::get(conn, &app_id, &endpoint_id)?;
        assert!(endpoint.is_disabled());
        assert!(endpoint.disabled_reason.is_some());
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn releasing_a_lease_costs_no_attempt() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        seed(conn, 1)?;
        let tx = conn.transaction()?;
        let job = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 1)?.remove(0);
        tx.commit()?;

        queue::release(conn, &job.delivery.id, NOW)?;
        let tx = conn.transaction()?;
        let again = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 1)?;
        tx.commit()?;
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].delivery.attempts, 0);
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn depth_reports_what_is_waiting() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        seed(conn, 5)?;
        let empty = queue::depth(conn, NOW - 1)?;
        assert_eq!(empty.due, 0, "nothing is due before it was created");

        let d = queue::depth(conn, NOW + 2_000)?;
        assert_eq!(d.pending, 5);
        assert_eq!(d.due, 5);
        assert_eq!(d.in_flight, 0);
        assert_eq!(d.oldest_due_age_ms, 2_000);

        let tx = conn.transaction()?;
        queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 2)?;
        tx.commit()?;
        let d = queue::depth(conn, NOW + 2_000)?;
        assert_eq!(d.in_flight, 2);
        assert_eq!(d.pending, 5, "leased work is still pending");
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_rate_limit_spaces_deliveries_out() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        // Four a minute: one every fifteen seconds.
        let (endpoint, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://example.com/hook",
            "",
            None,
            Some(4),
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        for i in 0..20 {
            let tx = conn.transaction()?;
            store::messages::create(&tx, &app.id, "x", &serde_json::json!({ "i": i }), None, NOW)?;
            tx.commit()?;
        }

        let spacing = queue::RATE_PERIOD / 4;

        // However many a worker asks for, it gets one.
        let tx = conn.transaction()?;
        let first = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert_eq!(
            first.len(),
            1,
            "a limited endpoint gives up one delivery at a time"
        );

        let tx = conn.transaction()?;
        let early = queue::claim(&tx, NOW + spacing - 1, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert!(early.is_empty(), "nothing before the spacing has elapsed");

        let tx = conn.transaction()?;
        let next = queue::claim(&tx, NOW + spacing, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;
        assert_eq!(next.len(), 1, "and one when it has");

        // Over a minute, no more than the limit. Walk a minute in small steps
        // and count what comes out.
        let mut delivered = 0;
        let mut at = NOW + spacing;
        for _ in 0..600 {
            at += 100;
            let tx = conn.transaction()?;
            delivered += queue::claim(&tx, at, queue::DEFAULT_LEASE, 100)?.len();
            tx.commit()?;
        }
        assert!(
            delivered <= 4,
            "{} deliveries went out in a minute against a limit of 4",
            delivered
        );
        assert!(
            delivered >= 3,
            "the limit should not throttle below itself: {}",
            delivered
        );
        let _ = endpoint;
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_rate_limit_reports_when_the_next_one_may_go() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        let (endpoint, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://example.com/hook",
            "",
            None,
            Some(6),
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        let tx = conn.transaction()?;
        store::messages::create(&tx, &app.id, "x", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;

        assert!(store::health::get(conn, &endpoint.id)?
            .next_allowed_at
            .is_none());
        let tx = conn.transaction()?;
        queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 10)?;
        tx.commit()?;
        assert_eq!(
            store::health::get(conn, &endpoint.id)?.next_allowed_at,
            Some(NOW + queue::RATE_PERIOD / 6)
        );
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn one_rate_limited_endpoint_does_not_hold_up_another() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        let (slow, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://slow.example/hook",
            "",
            Some(&["slow".to_string()]),
            Some(1),
            &sign::new_secret(),
            NOW,
        )?;
        let (fast, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://fast.example/hook",
            "",
            Some(&["fast".to_string()]),
            None,
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        for i in 0..5 {
            for event in ["slow", "fast"] {
                let tx = conn.transaction()?;
                store::messages::create(
                    &tx,
                    &app.id,
                    event,
                    &serde_json::json!({ "i": i }),
                    None,
                    NOW,
                )?;
                tx.commit()?;
            }
        }

        let tx = conn.transaction()?;
        let claimed = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 100)?;
        tx.commit()?;

        let to_slow = claimed.iter().filter(|j| j.endpoint.id == slow.id).count();
        let to_fast = claimed.iter().filter(|j| j.endpoint.id == fast.id).count();
        assert_eq!(to_slow, 1, "the limited endpoint is held to its limit");
        assert_eq!(to_fast, 5, "the unlimited one is not held up by it");
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_disabled_endpoint_is_never_claimed() {
    // Disabling cancels what is queued, so this is about the race: a delivery
    // queued in the moment between the check and the disable.
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let (app_id, endpoint_id) = seed(conn, 3)?;
        conn.execute(
            "UPDATE endpoints SET disabled_at = ?2 WHERE id = ?1",
            rusqlite::params![endpoint_id, NOW],
        )?;
        let tx = conn.transaction()?;
        let claimed = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 10)?;
        tx.commit()?;
        assert!(
            claimed.is_empty(),
            "a disabled endpoint must not be delivered to"
        );
        let _ = app_id;
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn idle_rate_limited_endpoints_do_not_starve_a_busy_one() {
    // The claim selected rate-limited endpoints that were merely *allowed*
    // another delivery, up to the batch size. Endpoints with nothing queued
    // filled the batch, and one with a backlog behind it was never reached.
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        for i in 0..30 {
            store::endpoints::create(
                &tx,
                &app.id,
                &format!("https://quiet{}.example/hook", i),
                "",
                Some(&["quiet".to_string()]),
                Some(60),
                &sign::new_secret(),
                NOW,
            )?;
        }
        let (busy, _) = store::endpoints::create(
            &tx,
            &app.id,
            "https://busy.example/hook",
            "",
            Some(&["loud".to_string()]),
            Some(60),
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;

        let tx = conn.transaction()?;
        store::messages::create(&tx, &app.id, "loud", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;

        let tx = conn.transaction()?;
        let claimed = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 16)?;
        tx.commit()?;
        assert_eq!(
            claimed.iter().filter(|j| j.endpoint.id == busy.id).count(),
            1,
            "the only endpoint with work should have been claimed, not crowded out by 30 idle ones"
        );
        Ok(())
    })
    .await
    .expect("queue");
}

#[tokio::test]
async fn a_stored_rate_limit_of_zero_still_drains() {
    // The API refuses zero. A row that carries one anyway — an older database,
    // a direct edit — must still be delivered to slowly rather than parked for
    // ever: a queue that silently never drains is the worst available
    // behaviour.
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        let app = store::apps::create(conn, "acme", None, &serde_json::json!({}), NOW)?;
        let tx = conn.transaction()?;
        store::endpoints::create(
            &tx,
            &app.id,
            "https://example.com/hook",
            "",
            None,
            Some(0),
            &sign::new_secret(),
            NOW,
        )?;
        tx.commit()?;
        let tx = conn.transaction()?;
        store::messages::create(&tx, &app.id, "x", &serde_json::json!({}), None, NOW)?;
        tx.commit()?;

        let tx = conn.transaction()?;
        let claimed = queue::claim(&tx, NOW, queue::DEFAULT_LEASE, 16)?;
        tx.commit()?;
        assert_eq!(claimed.len(), 1, "a zero limit must not mean never");
        Ok(())
    })
    .await
    .expect("queue");
}
