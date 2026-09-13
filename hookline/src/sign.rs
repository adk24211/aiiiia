//! Webhook signatures.
//!
//! The format is [Standard Webhooks], the same one Svix, Resend and others
//! emit, which matters for two reasons. A consumer can verify a hookline
//! webhook with a verification library they already have, in any language,
//! rather than reading a bespoke spec. And anyone migrating away from a hosted
//! provider does not have to ask every one of their customers to change code.
//!
//! [Standard Webhooks]: https://www.standardwebhooks.com
//!
//! Three headers go out with every request:
//!
//! ```text
//! webhook-id: msg_2vXk...            the message id, unique per message
//! webhook-timestamp: 1700000000      seconds since the epoch
//! webhook-signature: v1,<base64>     space-separated, one per active secret
//! ```
//!
//! The signed string is `{id}.{timestamp}.{body}` — the id and timestamp are
//! inside the signature, so neither can be altered, and the timestamp lets a
//! consumer reject a replayed request outside a tolerance window.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// The prefix a secret is stored and displayed with.
pub const SECRET_PREFIX: &str = "whsec_";

/// How far a timestamp may be from now before a consumer should reject it.
/// Five minutes each way is the Standard Webhooks recommendation.
pub const DEFAULT_TOLERANCE_SECS: i64 = 5 * 60;

/// Generate a signing secret: 24 random bytes, base64, with a `whsec_` prefix.
pub fn new_secret() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill(&mut bytes);
    format!(
        "{}{}",
        SECRET_PREFIX,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// The bytes a secret actually keys the HMAC with.
///
/// A `whsec_`-prefixed secret is base64 of the real key, which is what every
/// Standard Webhooks verifier expects. Anything else is used as raw bytes, so
/// a key imported from somewhere else still works.
fn key_bytes(secret: &str) -> Vec<u8> {
    match secret.strip_prefix(SECRET_PREFIX) {
        Some(encoded) => base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap_or_else(|_| encoded.as_bytes().to_vec()),
        None => secret.as_bytes().to_vec(),
    }
}

/// One `v1,<base64>` signature over `{id}.{timestamp}.{body}`.
pub fn sign_one(secret: &str, message_id: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key_bytes(secret))
        .expect("HMAC accepts a key of any length");
    mac.update(message_id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(tag)
    )
}

/// The `webhook-signature` header value for every active secret.
///
/// During a rotation an endpoint has more than one valid secret, and the
/// header carries a signature under each. The consumer accepts a request if
/// *any* of them verifies, which is what lets a secret be rotated without
/// coordinating a deploy on their side.
pub fn sign(secrets: &[String], message_id: &str, timestamp: i64, body: &[u8]) -> String {
    secrets
        .iter()
        .map(|s| sign_one(s, message_id, timestamp, body))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Verify a `webhook-signature` header, as a consumer would.
///
/// Shipped so that the test suite checks signatures the way a customer will,
/// and so the docs can point at a reference implementation that is compiled
/// and tested rather than pasted into a page.
pub fn verify(
    secret: &str,
    header: &str,
    message_id: &str,
    timestamp: i64,
    body: &[u8],
    now: i64,
    tolerance_secs: i64,
) -> Result<(), VerifyError> {
    if (now - timestamp).abs() > tolerance_secs {
        return Err(VerifyError::Timestamp);
    }
    let expected = sign_one(secret, message_id, timestamp, body);
    let expected = expected.as_bytes();
    // Every candidate is compared, and the comparison is constant time: an
    // early return on the first mismatching byte leaks how much of a forged
    // signature was right.
    let mut matched = 0u8;
    for candidate in header.split(' ') {
        if candidate.len() == expected.len() {
            matched |= candidate.as_bytes().ct_eq(expected).unwrap_u8();
        }
    }
    if matched == 1 {
        Ok(())
    } else {
        Err(VerifyError::Signature)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VerifyError {
    /// The timestamp is outside the tolerance window: a replay, or a clock
    /// that needs attention.
    Timestamp,
    /// No signature in the header verified under this secret.
    Signature,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Timestamp => f.write_str("timestamp outside the tolerance window"),
            VerifyError::Signature => f.write_str("no signature verified"),
        }
    }
}

impl std::error::Error for VerifyError {}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "msg_2vXkJ3Q1Zk5W8mN0PbR7TcYd";
    const NOW: i64 = 1_700_000_000;

    #[test]
    fn a_signature_verifies() {
        let secret = new_secret();
        let header = sign(&[secret.clone()], ID, NOW, b"{\"hello\":1}");
        assert!(header.starts_with("v1,"));
        verify(&secret, &header, ID, NOW, b"{\"hello\":1}", NOW, 300).expect("should verify");
    }

    #[test]
    fn every_part_of_the_request_is_covered() {
        let secret = new_secret();
        let body = b"{\"amount\":100}";
        let header = sign(&[secret.clone()], ID, NOW, body);

        // A different body, id or timestamp must not verify: all three are
        // inside the signed string.
        assert_eq!(
            verify(&secret, &header, ID, NOW, b"{\"amount\":900}", NOW, 300),
            Err(VerifyError::Signature)
        );
        assert_eq!(
            verify(&secret, &header, "msg_0000000000000000000000000", NOW, body, NOW, 300),
            Err(VerifyError::Signature)
        );
        assert_eq!(
            verify(&secret, &header, ID, NOW + 1, body, NOW + 1, 300),
            Err(VerifyError::Signature)
        );
        // And a different secret must not.
        assert_eq!(
            verify(&new_secret(), &header, ID, NOW, body, NOW, 300),
            Err(VerifyError::Signature)
        );
    }

    #[test]
    fn a_stale_timestamp_is_rejected_before_the_signature_is_checked() {
        let secret = new_secret();
        let header = sign(&[secret.clone()], ID, NOW, b"{}");
        assert_eq!(
            verify(&secret, &header, ID, NOW, b"{}", NOW + 301, 300),
            Err(VerifyError::Timestamp)
        );
        assert_eq!(
            verify(&secret, &header, ID, NOW, b"{}", NOW - 301, 300),
            Err(VerifyError::Timestamp)
        );
        verify(&secret, &header, ID, NOW, b"{}", NOW + 299, 300).expect("inside the window");
    }

    #[test]
    fn rotation_keeps_both_secrets_working() {
        // The point of rotation: sign with old and new at once, so a consumer
        // still on the old secret keeps verifying while they move.
        let old = new_secret();
        let new = new_secret();
        let header = sign(&[old.clone(), new.clone()], ID, NOW, b"{}");
        assert_eq!(header.split(' ').count(), 2);
        verify(&old, &header, ID, NOW, b"{}", NOW, 300).expect("old secret");
        verify(&new, &header, ID, NOW, b"{}", NOW, 300).expect("new secret");
        assert_eq!(
            verify(&new_secret(), &header, ID, NOW, b"{}", NOW, 300),
            Err(VerifyError::Signature)
        );
    }

    #[test]
    fn a_raw_secret_without_the_prefix_also_works() {
        // Someone importing a key from another provider should not have to
        // re-encode it.
        let header = sign(&["plain-text-key".into()], ID, NOW, b"{}");
        verify("plain-text-key", &header, ID, NOW, b"{}", NOW, 300).expect("raw key");
    }

    #[test]
    fn signatures_match_the_standard_webhooks_test_vector() {
        // From the Standard Webhooks reference implementations. Getting this
        // exactly right is the whole reason to use the format: a consumer's
        // existing library has to verify what we send.
        let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
        let id = "msg_p5jXN8AQM9LWM0D4loKWxJek";
        let timestamp = 1614265330;
        let body = br#"{"test": 2432232314}"#;
        assert_eq!(
            sign_one(secret, id, timestamp, body),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
    }
}
