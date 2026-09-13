//! A whole hookline, and a consumer for it to deliver to.
//!
//! The tests below run the real server on an ephemeral port and talk to it
//! over HTTP, because the interesting failures are the ones that live between
//! the pieces: a signature that is right in a unit test and wrong over the
//! wire, a retry that the worker schedules and the queue never hands back.

#![allow(dead_code)]

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use hookline::config::Config;
use hookline::db::Db;
use hookline::server::Server;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One request the consumer received.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// How the consumer should answer.
#[derive(Clone, Copy, Debug)]
pub enum Behaviour {
    Ok,
    /// Answer with this status.
    Status(u16),
    /// Fail this many times, then succeed.
    FailThen(usize),
    /// Take longer than any sane timeout.
    Hang,
}

/// A webhook consumer under the test's control.
pub struct Consumer {
    pub addr: SocketAddr,
    state: ConsumerState,
}

#[derive(Clone)]
struct ConsumerState {
    received: Arc<Mutex<Vec<Received>>>,
    behaviour: Arc<Mutex<Behaviour>>,
    calls: Arc<AtomicUsize>,
}

impl Consumer {
    pub async fn start() -> Consumer {
        let state = ConsumerState {
            received: Arc::new(Mutex::new(Vec::new())),
            behaviour: Arc::new(Mutex::new(Behaviour::Ok)),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/hook", post(handle))
            .route("/hook/:tag", post(handle))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Consumer { addr, state }
    }

    pub fn url(&self) -> String {
        format!("http://{}/hook", self.addr)
    }

    pub fn behave(&self, behaviour: Behaviour) {
        *self.state.behaviour.lock().unwrap() = behaviour;
    }

    pub fn received(&self) -> Vec<Received> {
        self.state.received.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.state.received.lock().unwrap().len()
    }

    /// Wait until at least `n` requests have arrived, or give up.
    pub async fn wait_for(&self, n: usize, within: Duration) -> Vec<Received> {
        let deadline = Instant::now() + within;
        loop {
            let got = self.received();
            if got.len() >= n {
                return got;
            }
            if Instant::now() >= deadline {
                panic!("expected {} requests, got {} in {:?}", n, got.len(), within);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn handle(
    State(state): State<ConsumerState>,
    request: axum::extract::Request,
) -> (StatusCode, &'static str) {
    let path = request.uri().path().to_string();
    let headers = request
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();

    let nth = state.calls.fetch_add(1, Ordering::SeqCst);
    state.received.lock().unwrap().push(Received {
        path,
        headers,
        body,
    });

    let behaviour = *state.behaviour.lock().unwrap();
    match behaviour {
        Behaviour::Ok => (StatusCode::OK, "ok"),
        Behaviour::Status(code) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "as asked",
        ),
        Behaviour::FailThen(n) if nth < n => (StatusCode::INTERNAL_SERVER_ERROR, "not yet"),
        Behaviour::FailThen(_) => (StatusCode::OK, "ok"),
        Behaviour::Hang => {
            tokio::time::sleep(Duration::from_secs(60)).await;
            (StatusCode::OK, "eventually")
        }
    }
}

/// A running hookline, with an admin token and a client that talks to it.
pub struct Harness {
    pub base: String,
    pub token: String,
    pub db: Db,
    pub client: reqwest::Client,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    _dir: ScratchDir,
}

impl Harness {
    pub async fn start() -> Harness {
        Harness::start_with(|c| c).await
    }

    /// Start with the configuration adjusted, for the tests that are about a
    /// setting rather than about behaviour.
    pub async fn start_with(tune: impl FnOnce(Config) -> Config) -> Harness {
        let dir = ScratchDir::new();
        let mut config = Config {
            database: dir.path.join("hookline.db"),
            // The delivery tests want the consumer on loopback, which the
            // default policy exists to refuse. The tests that are about that
            // policy build their own configuration.
            destinations: hookline::guard::Policy::permissive(),
            poll_interval: Duration::from_millis(20),
            request_timeout: Duration::from_secs(2),
            lease: Duration::from_secs(5),
            attempt_retention: None,
            ..Config::default()
        };
        config.retry.base = Duration::from_millis(40);
        config.retry.factor = 1.5;
        config.retry.max_delay = Duration::from_millis(200);
        config.retry.jitter = 0.0;
        config = tune(config);

        let server = Server::new(config).expect("build the server");
        let db = server.db.clone();
        let token = hookline::auth::mint(&db, "tests", hookline::auth::Scope::Admin)
            .await
            .expect("mint")
            .token;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = server
                .run_on(listener, async move {
                    let _ = rx.await;
                })
                .await;
        });

        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");

        let harness = Harness {
            base: format!("http://{}", addr),
            token,
            db,
            client,
            shutdown: Some(tx),
            _dir: dir,
        };
        harness.wait_until_up().await;
        harness
    }

    async fn wait_until_up(&self) {
        for _ in 0..200 {
            if self
                .client
                .get(format!("{}/health", self.base))
                .send()
                .await
                .is_ok()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the server never came up");
    }

    pub async fn post(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        self.request(reqwest::Method::POST, path, Some(body), &self.token)
            .await
    }

    pub async fn post_as(
        &self,
        token: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        self.request(reqwest::Method::POST, path, Some(body), token)
            .await
    }

    pub async fn get(&self, path: &str) -> (u16, serde_json::Value) {
        self.request(reqwest::Method::GET, path, None, &self.token)
            .await
    }

    pub async fn get_as(&self, token: &str, path: &str) -> (u16, serde_json::Value) {
        self.request(reqwest::Method::GET, path, None, token).await
    }

    pub async fn patch(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        self.request(reqwest::Method::PATCH, path, Some(body), &self.token)
            .await
    }

    pub async fn delete(&self, path: &str) -> (u16, serde_json::Value) {
        self.request(reqwest::Method::DELETE, path, None, &self.token)
            .await
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
        token: &str,
    ) -> (u16, serde_json::Value) {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base, path))
            .header("authorization", format!("Bearer {}", token));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .expect("the request should reach the server");
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
        (status, json)
    }

    /// Create an application and an endpoint pointing at `url`, and return
    /// `(app_id, endpoint_id, secret)`.
    pub async fn wire(
        &self,
        url: &str,
        event_types: Option<Vec<&str>>,
    ) -> (String, String, String) {
        let (status, app) = self
            .post("/v1/apps", serde_json::json!({ "name": "acme" }))
            .await;
        assert_eq!(status, 200, "{:?}", app);
        let app_id = app["id"].as_str().expect("an id").to_string();

        let mut body = serde_json::json!({ "url": url });
        if let Some(types) = event_types {
            body["event_types"] = serde_json::json!(types);
        }
        let (status, endpoint) = self
            .post(&format!("/v1/apps/{}/endpoints", app_id), body)
            .await;
        assert_eq!(status, 200, "{:?}", endpoint);
        (
            app_id,
            endpoint["id"].as_str().expect("an id").to_string(),
            endpoint["secret"].as_str().expect("a secret").to_string(),
        )
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// A scratch directory that cleans up after itself.
pub struct ScratchDir {
    pub path: std::path::PathBuf,
}

impl ScratchDir {
    pub fn new() -> ScratchDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "hookline-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create the scratch directory");
        ScratchDir { path }
    }
}

impl Default for ScratchDir {
    fn default() -> ScratchDir {
        ScratchDir::new()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
