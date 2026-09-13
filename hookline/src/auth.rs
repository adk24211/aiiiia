//! API credentials.
//!
//! A token is a long random string shown once. What is stored is its SHA-256
//! hash, so a copy of the database is not a copy of the credentials — the
//! property that matters when the database is a file people back up, copy to
//! a laptop to debug, and occasionally leave somewhere.
//!
//! There is no login, no session and no password. A service that sends
//! webhooks is talking to other services, and the credential for that is a
//! token in a header.

use crate::db::Db;
use crate::error::{Error, Result};
use crate::models::ApiKey;
use crate::store;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// The prefix every token carries, so one found in a log or a repository is
/// recognisable as a credential for this and can be revoked.
pub const TOKEN_PREFIX: &str = "hl_";

/// What a key is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everything, including creating and revoking other keys.
    Admin,
    /// Post messages, and read nothing else.
    ///
    /// This is the scope for the credential that lives in your application
    /// servers. They need to send events; they do not need to be able to
    /// rotate a signing secret or read another tenant's delivery history, and
    /// a leaked publishing token should not be able to.
    Publish,
    /// Read-only: listings and history, no writes.
    Read,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Admin => "admin",
            Scope::Publish => "publish",
            Scope::Read => "read",
        }
    }

    pub fn parse(s: &str) -> Result<Scope> {
        match s {
            "admin" => Ok(Scope::Admin),
            "publish" => Ok(Scope::Publish),
            "read" => Ok(Scope::Read),
            other => Err(Error::invalid(format!(
                "unknown scope {:?}; expected admin, publish or read",
                other
            ))),
        }
    }

    /// Whether this scope may make a change of any kind.
    pub fn may_write(self) -> bool {
        matches!(self, Scope::Admin)
    }

    /// Whether this scope may post messages.
    pub fn may_publish(self) -> bool {
        matches!(self, Scope::Admin | Scope::Publish)
    }

    /// Whether this scope may read history and configuration.
    pub fn may_read(self) -> bool {
        matches!(self, Scope::Admin | Scope::Read)
    }
}

/// A freshly minted token and the record that will authenticate it.
pub struct Minted {
    pub key: ApiKey,
    /// The only time this exists. It is not recoverable afterwards.
    pub token: String,
}

/// Generate a token.
///
/// 256 bits from the operating system's generator. Long enough that guessing
/// is not a threat model, and no cleverness: a token with structure is a token
/// with a smaller keyspace than it looks like it has.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "{}{}",
        TOKEN_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// The part of a token shown in listings.
///
/// Enough to tell two keys apart, far too little to be worth guessing the
/// rest from.
pub fn display_prefix(token: &str) -> String {
    token.chars().take(TOKEN_PREFIX.len() + 6).collect()
}

/// Create a key and return the token, which is not stored.
pub async fn mint(db: &Db, name: &str, scope: Scope) -> Result<Minted> {
    let token = new_token();
    let hashed = hash(&token);
    let prefix = display_prefix(&token);
    let name = name.to_string();
    let now = crate::now_millis();
    let key = db
        .call(move |conn| store::keys::create(conn, &name, &hashed, &prefix, scope.as_str(), now))
        .await?;
    Ok(Minted { key, token })
}

/// Who a request is.
#[derive(Debug, Clone)]
pub struct Identity {
    pub key_id: String,
    pub scope: Scope,
}

impl Identity {
    pub fn require_write(&self) -> Result<()> {
        if self.scope.may_write() {
            return Ok(());
        }
        Err(Error::Forbidden(format!(
            "this key has the {} scope, which cannot make changes",
            self.scope.as_str()
        )))
    }

    pub fn require_publish(&self) -> Result<()> {
        if self.scope.may_publish() {
            return Ok(());
        }
        Err(Error::Forbidden(format!(
            "this key has the {} scope, which cannot post messages",
            self.scope.as_str()
        )))
    }

    pub fn require_read(&self) -> Result<()> {
        if self.scope.may_read() {
            return Ok(());
        }
        Err(Error::Forbidden(format!(
            "this key has the {} scope, which cannot read",
            self.scope.as_str()
        )))
    }
}

/// Find the identity a token names.
///
/// The lookup is by hash, so the stored value is never compared against
/// anything an attacker controls the timing of, and a revoked key is excluded
/// by the query rather than by a check the caller could forget.
pub async fn authenticate(db: &Db, token: &str) -> Result<Identity> {
    let hashed = hash(token);
    let key = db
        .call(move |conn| store::keys::by_hash(conn, &hashed))
        .await?
        .ok_or(Error::Unauthorized)?;
    let scope = Scope::parse(&key.scope).unwrap_or(Scope::Read);

    let id = key.id.clone();
    let db = db.clone();
    // Last-used is bookkeeping, not part of the answer: a failure to write it
    // must not fail the request that was otherwise authenticated.
    tokio::spawn(async move {
        let now = crate::now_millis();
        let _ = db
            .call(move |conn| store::keys::touch(conn, &id, now))
            .await;
    });

    Ok(Identity {
        key_id: key.id,
        scope,
    })
}

/// Pull the token out of the headers.
///
/// `Authorization: Bearer <token>` is the standard spelling and what every
/// client library does by default. A bare token is also accepted, because the
/// first thing anyone does is paste it into curl.
pub fn token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_long_and_prefixed() {
        let token = new_token();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert!(
            token.len() >= 40,
            "{} is too short to be a credential",
            token
        );
    }

    #[test]
    fn tokens_do_not_repeat() {
        let seen: std::collections::HashSet<String> = (0..5_000).map(|_| new_token()).collect();
        assert_eq!(seen.len(), 5_000);
    }

    #[test]
    fn the_hash_is_stable_and_not_the_token() {
        let token = new_token();
        assert_eq!(hash(&token), hash(&token));
        assert_ne!(hash(&token), token);
        assert_eq!(hash(&token).len(), 64);
        assert_ne!(hash(&token), hash(&new_token()));
    }

    #[test]
    fn the_displayed_prefix_reveals_almost_nothing() {
        let token = new_token();
        let prefix = display_prefix(&token);
        assert!(token.starts_with(&prefix));
        assert!(prefix.len() < token.len() / 3);
    }

    #[test]
    fn a_publishing_key_cannot_read_or_change_anything() {
        let id = Identity {
            key_id: "key_1".into(),
            scope: Scope::Publish,
        };
        assert!(id.require_publish().is_ok());
        assert!(id.require_read().is_err());
        assert!(id.require_write().is_err());
    }

    #[test]
    fn a_reading_key_cannot_publish() {
        let id = Identity {
            key_id: "key_1".into(),
            scope: Scope::Read,
        };
        assert!(id.require_read().is_ok());
        assert!(id.require_publish().is_err());
        assert!(id.require_write().is_err());
    }

    #[test]
    fn an_admin_key_may_do_everything() {
        let id = Identity {
            key_id: "key_1".into(),
            scope: Scope::Admin,
        };
        assert!(id.require_read().is_ok());
        assert!(id.require_publish().is_ok());
        assert!(id.require_write().is_ok());
    }

    #[test]
    fn the_header_is_read_with_or_without_bearer() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("authorization", "Bearer hl_abc".parse().unwrap());
        assert_eq!(token_from_headers(&headers).as_deref(), Some("hl_abc"));
        headers.insert("authorization", "hl_abc".parse().unwrap());
        assert_eq!(token_from_headers(&headers).as_deref(), Some("hl_abc"));
        headers.insert("authorization", "Bearer   ".parse().unwrap());
        assert_eq!(token_from_headers(&headers), None);
        assert_eq!(token_from_headers(&axum::http::HeaderMap::new()), None);
    }
}
