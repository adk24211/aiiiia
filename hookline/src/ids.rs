//! Prefixed, time-sortable identifiers.
//!
//! Every id carries a prefix naming what it is (`msg_`, `ep_`), so an id
//! pasted into a support ticket is self-describing and a caller cannot pass an
//! endpoint id where a message id belongs. The body is a ULID: 48 bits of
//! millisecond timestamp followed by 80 bits of randomness, in Crockford
//! base32.
//!
//! Sortability is not cosmetic. Ids are primary keys in SQLite, and monotonic
//! keys keep B-tree inserts appending at the right edge instead of splitting
//! pages all over the index, which is the difference between a queue table
//! that stays fast and one that does not.

use rand::Rng;
use std::fmt;

/// Crockford base32: no I, L, O or U, so an id read aloud or typed by hand
/// cannot be confused between characters.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// What an id refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Application,
    Endpoint,
    Message,
    Delivery,
    Attempt,
    Secret,
    ApiKey,
}

impl Kind {
    pub const fn prefix(self) -> &'static str {
        match self {
            Kind::Application => "app",
            Kind::Endpoint => "ep",
            Kind::Message => "msg",
            Kind::Delivery => "dlv",
            Kind::Attempt => "att",
            Kind::Secret => "sec",
            Kind::ApiKey => "key",
        }
    }

    pub fn from_prefix(s: &str) -> Option<Kind> {
        Some(match s {
            "app" => Kind::Application,
            "ep" => Kind::Endpoint,
            "msg" => Kind::Message,
            "dlv" => Kind::Delivery,
            "att" => Kind::Attempt,
            "sec" => Kind::Secret,
            "key" => Kind::ApiKey,
            _ => return None,
        })
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.prefix())
    }
}

/// Generate an id of the given kind.
pub fn new(kind: Kind) -> String {
    new_at(kind, crate::now_millis())
}

/// Generate an id stamped with a specific time.
///
/// Milliseconds are signed everywhere in this crate so that a duration between
/// two of them is plain subtraction; only the low 48 bits are encoded, which
/// runs out in the year 10889.
pub fn new_at(kind: Kind, millis: i64) -> String {
    let millis = millis.max(0) as u64;
    let mut raw = [0u8; 16];
    raw[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    rand::thread_rng().fill(&mut raw[6..]);

    let mut out = String::with_capacity(kind.prefix().len() + 27);
    out.push_str(kind.prefix());
    out.push('_');
    encode_into(&raw, &mut out);
    out
}

/// Crockford base32 of 16 bytes, most significant bit first.
fn encode_into(raw: &[u8; 16], out: &mut String) {
    let mut bits: u32 = 0;
    let mut held: u32 = 0;
    for &byte in raw {
        bits = (bits << 8) | byte as u32;
        held += 8;
        while held >= 5 {
            held -= 5;
            out.push(ALPHABET[((bits >> held) & 0x1f) as usize] as char);
        }
    }
    if held > 0 {
        out.push(ALPHABET[((bits << (5 - held)) & 0x1f) as usize] as char);
    }
}

/// Is this a well-formed id of the given kind?
///
/// Checked on the way in so a malformed id becomes a 400 at the edge rather
/// than a confusing empty result from a query.
pub fn is(kind: Kind, id: &str) -> bool {
    let Some(body) = id
        .strip_prefix(kind.prefix())
        .and_then(|r| r.strip_prefix('_'))
    else {
        return false;
    };
    body.len() == 26 && body.bytes().all(|b| ALPHABET.contains(&b))
}

/// The kind an id claims to be, if it is well formed.
pub fn kind_of(id: &str) -> Option<Kind> {
    let (prefix, _) = id.split_once('_')?;
    let kind = Kind::from_prefix(prefix)?;
    is(kind, id).then_some(kind)
}

/// The millisecond timestamp encoded in an id.
pub fn time_of(id: &str) -> Option<i64> {
    let body = id.split_once('_')?.1;
    if body.len() != 26 {
        return None;
    }
    let mut millis: u64 = 0;
    for c in body.bytes().take(10) {
        let value = ALPHABET.iter().position(|&a| a == c)? as u64;
        millis = (millis << 5) | value;
    }
    // Ten base32 characters carry fifty bits; the timestamp is the low 48.
    Some((millis >> 2) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_carry_their_kind() {
        let id = new(Kind::Message);
        assert!(id.starts_with("msg_"), "{}", id);
        assert!(is(Kind::Message, &id));
        assert!(!is(Kind::Endpoint, &id));
        assert_eq!(kind_of(&id), Some(Kind::Message));
    }

    #[test]
    fn malformed_ids_are_rejected() {
        for bad in [
            "",
            "msg_",
            "msg",
            "msg_short",
            "msg_000000000000000000000000000", // twenty-seven characters
            "msg_IIIIIIIIIIIIIIIIIIIIIIIIII",  // I is not in the alphabet
            "nope_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        ] {
            assert!(!is(Kind::Message, bad), "accepted `{}`", bad);
            assert_eq!(kind_of(bad), None, "accepted `{}`", bad);
        }
    }

    #[test]
    fn ids_sort_by_time() {
        // Lexical order has to match creation order, because the queue reads
        // rows in primary-key order and callers page through by id.
        let mut ids: Vec<String> = (0..64)
            .map(|i| new_at(Kind::Message, 1_700_000_000_000 + i * 1_000))
            .collect();
        let sorted = {
            let mut c = ids.clone();
            c.sort();
            c
        };
        assert_eq!(ids, sorted);
        ids.dedup();
        assert_eq!(ids.len(), 64);
    }

    #[test]
    fn ids_do_not_repeat() {
        let seen: HashSet<String> = (0..20_000).map(|_| new(Kind::Delivery)).collect();
        assert_eq!(seen.len(), 20_000);
    }

    #[test]
    fn the_timestamp_survives_a_round_trip() {
        for stamp in [0i64, 1, 1_700_000_000_000, (1i64 << 48) - 1] {
            let id = new_at(Kind::Attempt, stamp);
            assert_eq!(time_of(&id), Some(stamp), "{}", id);
        }
    }
}
