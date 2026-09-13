//! The `hookline` command.
//!
//! Four things: run the server, apply migrations, manage credentials, and
//! verify a signature. The last is here because the question "why is my
//! consumer rejecting this" is the most common support question a webhook
//! sender gets, and answering it should not require writing a script.

use hookline::auth::{self, Scope};
use hookline::config::Config;
use hookline::db::Db;
use hookline::server::Server;
use hookline::{sign, store};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("serve");

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(format!("cannot start the runtime: {}", e)),
    };

    let result = match command {
        "serve" => runtime.block_on(serve()),
        "migrate" => runtime.block_on(migrate()),
        "key" | "keys" => runtime.block_on(keys(&args[1..])),
        "verify" => verify(&args[1..]),
        "health" => runtime.block_on(health()),
        "version" | "--version" | "-V" => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print!("{}", USAGE);
            Ok(())
        }
        other => Err(format!("unknown command {:?}\n\n{}", other, USAGE)),
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn fail(message: String) -> std::process::ExitCode {
    eprintln!("hookline: {}", message);
    std::process::ExitCode::FAILURE
}

const USAGE: &str = "\
hookline — reliable webhook delivery

    hookline serve                     run the server (the default)
    hookline migrate                   create or update the database and exit
    hookline health                    ask a running server whether it is well
    hookline key create <name> [scope] mint a credential; scope is admin,
                                       publish (the default) or read
    hookline key list                  list credentials
    hookline key revoke <id>           revoke one
    hookline verify <secret> <id> <timestamp> <signature> <body>
                                       check a signature the way a consumer
                                       would, and say why it failed

Configuration is read from the environment; every setting has a default.

    HOOKLINE_LISTEN                    address to bind (0.0.0.0:8080)
    HOOKLINE_DATABASE                  database file (hookline.db)
    HOOKLINE_CONCURRENCY               deliveries in flight at once (32)
    HOOKLINE_MAX_ATTEMPTS              attempts before giving up (10)
    HOOKLINE_REQUEST_TIMEOUT_SECS      per-attempt timeout (15)
    HOOKLINE_ALLOW_PRIVATE_DESTINATIONS
                                       allow loopback and private addresses;
                                       off, and leave it off in production
    HOOKLINE_ALLOW_HTTP                allow plaintext destinations (off)

Run `hookline serve` with no database and it will create one.
";

fn configure() -> Result<Config, String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HOOKLINE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    Config::from_env()
}

async fn serve() -> Result<(), String> {
    let config = configure()?;
    tracing::info!("{}", config.summary());

    let server = Server::new(config)?;
    // A server nobody can authenticate against is a server that does nothing,
    // and the reason is not otherwise visible: every request simply answers
    // 401. Say so once, at the only moment it is useful to hear it.
    let any = server
        .db
        .call(|conn| store::keys::any(conn))
        .await
        .map_err(|e| format!("cannot read the database: {}", e))?;
    if !any {
        tracing::warn!(
            "no API keys exist, so every request will be refused; \
             mint one with `hookline key create <name> admin`"
        );
    }
    server.run().await
}

/// Ask a running server for its health route.
///
/// Here so that a container health check needs no `curl` in the image: an
/// image that carries a shell and an HTTP client to check on itself has a
/// larger attack surface than the thing it is checking.
async fn health() -> Result<(), String> {
    let config = Config::from_env()?;
    let url = format!("http://{}/health", config.listen);
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{} is not answering: {}", url, e))?;
    if !response.status().is_success() {
        return Err(format!("{} answered {}", url, response.status()));
    }
    println!("{}", response.text().await.unwrap_or_default());
    Ok(())
}

async fn migrate() -> Result<(), String> {
    let config = Config::from_env()?;
    Db::open(&config.database, 1)
        .map_err(|e| format!("cannot open {}: {}", config.database.display(), e))?;
    println!("{} is up to date", config.database.display());
    Ok(())
}

async fn keys(args: &[String]) -> Result<(), String> {
    let config = Config::from_env()?;
    let db = Db::open(&config.database, 1)
        .map_err(|e| format!("cannot open {}: {}", config.database.display(), e))?;

    match args.first().map(String::as_str) {
        Some("create") => {
            let name = args
                .get(1)
                .ok_or("usage: hookline key create <name> [scope]")?;
            let scope = match args.get(2) {
                Some(s) => Scope::parse(s).map_err(|e| e.to_string())?,
                None => Scope::Publish,
            };
            let minted = auth::mint(&db, name, scope)
                .await
                .map_err(|e| format!("cannot create the key: {}", e))?;
            println!("{}", minted.token);
            eprintln!(
                "\nid {}   scope {}\nThis token is not stored and cannot be shown again.",
                minted.key.id,
                scope.as_str()
            );
            Ok(())
        }
        Some("list") => {
            let keys = db
                .call(|conn| store::keys::list(conn))
                .await
                .map_err(|e| format!("cannot read keys: {}", e))?;
            if keys.is_empty() {
                println!("no keys");
                return Ok(());
            }
            for key in keys {
                println!(
                    "{}  {:<10} {:<24} {}",
                    key.id,
                    key.scope,
                    key.name,
                    if key.revoked_at.is_some() {
                        "revoked"
                    } else {
                        key.prefix.as_str()
                    }
                );
            }
            Ok(())
        }
        Some("revoke") => {
            let id = args
                .get(1)
                .ok_or("usage: hookline key revoke <id>")?
                .clone();
            let now = hookline::now_millis();
            db.call(move |conn| store::keys::revoke(conn, &id, now))
                .await
                .map_err(|e| format!("cannot revoke: {}", e))?;
            println!("revoked");
            Ok(())
        }
        _ => Err("usage: hookline key create|list|revoke".to_string()),
    }
}

/// Verify a signature exactly as a consumer's library would.
///
/// Prints which of the two things went wrong — the timestamp or the signature
/// — because "it does not verify" is the least useful possible answer, and the
/// two have completely different causes.
fn verify(args: &[String]) -> Result<(), String> {
    let [secret, id, timestamp, signature, body] = args else {
        return Err(
            "usage: hookline verify <secret> <message-id> <timestamp> <signature> <body>"
                .to_string(),
        );
    };
    let timestamp: i64 = timestamp
        .parse()
        .map_err(|_| format!("{:?} is not a unix timestamp in seconds", timestamp))?;

    let now = hookline::now_millis() / 1000;
    match sign::verify(
        secret,
        signature,
        id,
        timestamp,
        body.as_bytes(),
        now,
        sign::DEFAULT_TOLERANCE_SECS,
    ) {
        Ok(()) => {
            println!("valid");
            Ok(())
        }
        Err(sign::VerifyError::Timestamp) => Err(format!(
            "the timestamp is outside the {} second tolerance: it says {}, and it is now {} \
             ({} seconds apart). The signature itself was not checked.",
            sign::DEFAULT_TOLERANCE_SECS,
            timestamp,
            now,
            (now - timestamp).abs()
        )),
        Err(sign::VerifyError::Signature) => {
            let expected = sign::sign_one(secret, id, timestamp, body.as_bytes());
            Err(format!(
                "the signature does not match.\n  given    {}\n  expected {}\n\n\
                 The signed string is `{{id}}.{{timestamp}}.{{body}}`, and the body must be \
                 the exact bytes that were sent: a body that has been parsed and re-serialised \
                 is a different body.",
                signature, expected
            ))
        }
    }
}
