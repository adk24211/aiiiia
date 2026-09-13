//! Storage: a SQLite database and a pool of connections to it.
//!
//! SQLite is the right choice here and not a compromise. A webhook sender's
//! working set is a queue and an audit log, both of which are written once and
//! read by id; there is no query a separate database server would answer
//! faster than a local file, and the operational difference between "copy one
//! file" and "run Postgres" is the difference between a product one person can
//! deploy and one they cannot.
//!
//! Three settings make it safe under concurrency: WAL, so readers never block
//! the writer; a busy timeout, so a writer waits instead of returning
//! `SQLITE_BUSY`; and `synchronous = NORMAL`, which under WAL still survives a
//! process crash and only risks the last commits on power loss.

use crate::error::{Error, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// Every migration, in order. A migration is never edited once released;
/// changing the schema means appending another one.
const MIGRATIONS: &[(&str, &str)] = &[("0001_initial", include_str!("schema/0001_initial.sql"))];

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

impl Db {
    /// Open (or create) the database at `path` and bring the schema up to date.
    ///
    /// `path` may be `:memory:`, in which case the pool is a single connection:
    /// each in-memory connection is its own database, so a pool of them would
    /// be a pool of unrelated databases.
    pub fn open(path: impl AsRef<Path>, pool_size: usize) -> Result<Db> {
        let path = path.as_ref().to_path_buf();
        let in_memory = path.as_os_str() == ":memory:";
        let size = if in_memory { 1 } else { pool_size.max(1) };

        if let Some(parent) = path.parent() {
            if !in_memory && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::internal(format!("cannot create {:?}: {}", parent, e)))?;
            }
        }

        let mut first = open_one(&path)?;
        migrate(&mut first)?;

        let mut idle = Vec::with_capacity(size);
        idle.push(first);
        for _ in 1..size {
            idle.push(open_one(&path)?);
        }

        Ok(Db {
            inner: Arc::new(Inner {
                path,
                idle: Mutex::new(idle),
                permits: Arc::new(Semaphore::new(size)),
            }),
        })
    }

    /// A database that lives only as long as the handle, for tests.
    pub fn in_memory() -> Result<Db> {
        Db::open(":memory:", 1)
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Run `f` against a pooled connection on a blocking thread.
    ///
    /// Every query goes through here. rusqlite is synchronous, and calling it
    /// directly from an async task would block a runtime worker for the
    /// duration of the query — which is fine until the day a query is slow,
    /// and then it is an outage.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        // An owned permit, so it can travel into the blocking task and be
        // released when the task ends rather than when this future does: a
        // cancelled caller must not free a permit whose connection is still out.
        let permit = Arc::clone(&self.inner.permits)
            .acquire_owned()
            .await
            .map_err(|_| Error::internal("the connection pool is closed"))?;

        tokio::task::spawn_blocking(move || {
            let mut conn = {
                let mut idle = inner.idle.lock().unwrap_or_else(|e| e.into_inner());
                idle.pop()
            }
            .ok_or_else(|| Error::internal("the connection pool is empty"))?;

            let outcome = f(&mut conn);

            // The connection goes back whatever happened: a failed query does
            // not make a connection unusable, and dropping it would shrink the
            // pool permanently while the semaphore kept handing out permits.
            inner
                .idle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(conn);
            drop(permit);
            outcome
        })
        .await
        .map_err(|e| Error::internal(format!("database task failed: {}", e)))?
    }
}

fn open_one(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Wait rather than fail when another connection holds the write lock.
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(conn)
}

/// Apply every migration that has not run yet.
fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(
            name TEXT PRIMARY KEY,
            applied_at INTEGER NOT NULL
         )",
    )?;

    for (name, sql) in MIGRATIONS {
        let already: i64 = conn.query_row(
            "SELECT count(*) FROM schema_migrations WHERE name = ?1",
            [name],
            |row| row.get(0),
        )?;
        if already > 0 {
            continue;
        }
        // Each migration and its bookkeeping commit together, so a crash
        // halfway cannot leave a migration applied but unrecorded.
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations(name, applied_at) VALUES (?1, ?2)",
            rusqlite::params![name, crate::now_millis()],
        )?;
        tx.commit()?;
        tracing::info!(migration = name, "applied migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fresh_database_has_the_schema() {
        let db = Db::in_memory().expect("open");
        let tables: Vec<String> = db
            .call(|conn| {
                let mut stmt =
                    conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .expect("query");
        for expected in [
            "api_keys",
            "apps",
            "attempts",
            "deliveries",
            "endpoint_health",
            "endpoint_secrets",
            "endpoints",
            "messages",
            "schema_migrations",
        ] {
            assert!(tables.contains(&expected.to_string()), "missing {}", expected);
        }
    }

    #[tokio::test]
    async fn migrating_twice_changes_nothing() {
        let dir = tempdir();
        let path = dir.join("hookline.db");
        let db = Db::open(&path, 2).expect("open");
        db.call(|c| {
            c.execute("INSERT INTO apps(id, name, created_at) VALUES ('app_x', 'n', 1)", [])?;
            Ok(())
        })
        .await
        .expect("insert");
        drop(db);

        let again = Db::open(&path, 2).expect("reopen");
        let count: i64 = again
            .call(|c| Ok(c.query_row("SELECT count(*) FROM apps", [], |r| r.get(0))?))
            .await
            .expect("count");
        assert_eq!(count, 1, "reopening lost or duplicated data");
        let migrations: i64 = again
            .call(|c| Ok(c.query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))?))
            .await
            .expect("count");
        assert_eq!(migrations, MIGRATIONS.len() as i64);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_pool_survives_a_failing_query() {
        // A connection must go back to the pool even when the query errored,
        // or the pool shrinks to nothing under any load that includes a
        // constraint violation.
        let dir = tempdir();
        let db = Db::open(dir.join("pool.db"), 2).expect("open");
        db.call(|c| {
            c.execute("INSERT INTO apps(id, name, created_at) VALUES ('app_x', 'n', 1)", [])?;
            Ok(())
        })
        .await
        .expect("insert");

        for _ in 0..8 {
            let bad = db
                .call(|c| {
                    c.execute("INSERT INTO apps(id, name, created_at) VALUES ('app_x', 'n', 1)", [])?;
                    Ok(())
                })
                .await;
            assert!(bad.is_err(), "a duplicate primary key should be rejected");
        }
        let ok: i64 = db
            .call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get(0))?))
            .await
            .expect("the pool should still work");
        assert_eq!(ok, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn concurrent_writers_do_not_collide() {
        // WAL plus a busy timeout should turn contention into waiting rather
        // than into SQLITE_BUSY errors.
        let db = Db::open(tempdir().join("concurrent.db"), 4).expect("open");
        let mut tasks = Vec::new();
        for i in 0..40 {
            let db = db.clone();
            tasks.push(tokio::spawn(async move {
                db.call(move |c| {
                    c.execute(
                        "INSERT INTO apps(id, name, created_at) VALUES (?1, ?2, ?3)",
                        rusqlite::params![format!("app_{:04}", i), "n", 1],
                    )?;
                    Ok(())
                })
                .await
            }));
        }
        for t in tasks {
            t.await.expect("join").expect("insert");
        }
        let count: i64 = db
            .call(|c| Ok(c.query_row("SELECT count(*) FROM apps", [], |r| r.get(0))?))
            .await
            .expect("count");
        assert_eq!(count, 40);
    }

    /// A unique scratch directory. Writing one rather than taking a dependency
    /// on a temp-file crate for four call sites.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hookline-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch directory");
        dir
    }
}
