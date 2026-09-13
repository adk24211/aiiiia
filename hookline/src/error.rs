//! One error type, and what each variant means to a caller.
//!
//! Every variant maps to exactly one HTTP status, chosen so that a client can
//! act on the status alone: retry a 503, fix the request on a 400, stop on a
//! 409. The body is always the same shape, because a caller parsing errors
//! should not have to branch on which handler produced them.

use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// The request is malformed or asks for something impossible.
    Invalid(String),
    /// The thing referred to does not exist.
    NotFound(String),
    /// The request conflicts with what is already there.
    Conflict(String),
    /// No credentials, or credentials that are not valid.
    Unauthorized,
    /// Valid credentials that do not permit this.
    Forbidden(String),
    /// Too many requests.
    RateLimited { retry_after_secs: u64 },
    /// Storage failed. Usually transient; the caller may retry.
    Database(rusqlite::Error),
    /// Anything else, which is a bug here rather than in the request.
    Internal(String),
}

impl Error {
    pub fn invalid(message: impl Into<String>) -> Error {
        Error::Invalid(message.into())
    }
    pub fn not_found(what: impl Into<String>) -> Error {
        Error::NotFound(what.into())
    }
    pub fn conflict(message: impl Into<String>) -> Error {
        Error::Conflict(message.into())
    }
    pub fn internal(message: impl Into<String>) -> Error {
        Error::Internal(message.into())
    }

    pub fn status(&self) -> u16 {
        match self {
            Error::Invalid(_) => 400,
            Error::Unauthorized => 401,
            Error::Forbidden(_) => 403,
            Error::NotFound(_) => 404,
            Error::Conflict(_) => 409,
            Error::RateLimited { .. } => 429,
            Error::Database(_) | Error::Internal(_) => 500,
        }
    }

    /// A stable machine-readable code. Clients branch on this, not on the
    /// message, so the messages stay free to improve.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Invalid(_) => "invalid_request",
            Error::Unauthorized => "unauthorized",
            Error::Forbidden(_) => "forbidden",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::RateLimited { .. } => "rate_limited",
            Error::Database(_) => "storage_error",
            Error::Internal(_) => "internal_error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Error::Invalid(m) | Error::Forbidden(m) | Error::Conflict(m) | Error::Internal(m) => {
                m.clone()
            }
            Error::NotFound(what) => format!("no such {}", what),
            Error::Unauthorized => "missing or invalid credentials".into(),
            Error::RateLimited { retry_after_secs } => {
                format!("rate limited; retry in {}s", retry_after_secs)
            }
            // The caller gets no detail: a database message can name tables,
            // columns and values, and none of that is theirs to see. The full
            // error is logged where it is constructed.
            Error::Database(_) => "a storage operation failed".into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(e) => write!(f, "storage: {}", e),
            other => f.write_str(&other.message()),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound("record".into()),
            other => Error::Database(other),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error::Invalid(format!("malformed JSON: {}", e))
    }
}

/// The body of every error response.
#[derive(Serialize)]
pub struct Body {
    pub error: Detail,
}

#[derive(Serialize)]
pub struct Detail {
    pub code: &'static str,
    pub message: String,
}

impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        if let Error::Database(e) = &self {
            tracing::error!(error = %e, "storage operation failed");
        }
        if let Error::Internal(m) = &self {
            tracing::error!(error = %m, "internal error");
        }
        let status = axum::http::StatusCode::from_u16(self.status())
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (
            status,
            axum::Json(Body {
                error: Detail {
                    code: self.code(),
                    message: self.message(),
                },
            }),
        )
            .into_response();
        if let Error::RateLimited { retry_after_secs } = self {
            if let Ok(value) = retry_after_secs.to_string().parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }
        response
    }
}
