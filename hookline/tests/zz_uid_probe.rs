use hookline::{db::Db, store};

#[tokio::test]
async fn uid_set_later_can_shadow_another_apps_id() {
    let db = Db::in_memory().expect("open");
    db.call(|conn| {
        // b is created FIRST (lower rowid), a second.
        let b = store::apps::create(conn, "beta", Some("beta-uid"), &serde_json::json!({}), 1)?;
        let a = store::apps::create(conn, "alpha", None, &serde_json::json!({}), 2)?;
        // Now point b's uid at a's id.
        let b = store::apps::update(conn, &b.id, None, Some(&a.id), None)?;
        println!("a = {}  b = {} (uid {:?})", a.id, b.id, b.uid);
        let resolved = store::apps::get(conn, &a.id)?;
        println!("GET /apps/{} resolves to {} ({})", a.id, resolved.id, resolved.name);
        assert_eq!(resolved.id, a.id, "SHADOWED: another app's uid won");
        Ok(())
    })
    .await
    .expect("store");
}
