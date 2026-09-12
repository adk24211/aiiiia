//! Global string interning and a total-order key for `f64`.
//!
//! E-nodes must be `Copy + Eq + Hash + Ord` so that the e-graph can hashcons
//! them cheaply and iterate deterministically. Rust's `f64` is neither `Eq`
//! nor `Hash` (NaN != NaN, and `-0.0 == 0.0` while their bit patterns differ),
//! and `String` is not `Copy`. This module fixes both.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// An interned string. Comparison and hashing are `u32` operations.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym(u32);

struct Interner {
    ids: HashMap<&'static str, u32>,
    strs: Vec<&'static str>,
}

fn interner() -> &'static Mutex<Interner> {
    static I: OnceLock<Mutex<Interner>> = OnceLock::new();
    I.get_or_init(|| {
        Mutex::new(Interner {
            ids: HashMap::new(),
            strs: Vec::new(),
        })
    })
}

impl Sym {
    /// Intern `s`, returning a stable id for the process lifetime.
    pub fn new(s: &str) -> Sym {
        let mut i = interner().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&id) = i.ids.get(s) {
            return Sym(id);
        }
        // Interned strings live for the whole process; leaking is the point.
        let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
        let id = i.strs.len() as u32;
        i.strs.push(leaked);
        i.ids.insert(leaked, id);
        Sym(id)
    }

    /// The string this symbol was interned from.
    pub fn as_str(self) -> &'static str {
        let i = interner().lock().unwrap_or_else(|e| e.into_inner());
        i.strs[self.0 as usize]
    }

    /// The raw interner index.
    pub fn index(self) -> u32 {
        self.0
    }
}

impl From<&str> for Sym {
    fn from(s: &str) -> Sym {
        Sym::new(s)
    }
}

impl From<String> for Sym {
    fn from(s: String) -> Sym {
        Sym::new(&s)
    }
}

impl fmt::Display for Sym {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Sym {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------

/// A hashable, totally-ordered `f64` wrapper.
///
/// Equality is on the bit pattern, with one normalization: every NaN maps to a
/// single canonical NaN, because `Eq` demands reflexivity and IEEE-754 forbids
/// it. Nothing in the language can observe a NaN's sign or payload, so that
/// collapse is invisible.
///
/// `-0.0` is deliberately *not* collapsed into `0.0`, even though they compare
/// equal. They are distinguishable: `1 / -0.0` is `-inf` while `1 / 0.0` is
/// `+inf`, and `atan2` sees the difference too. Hashconsing them together
/// would let the e-graph silently substitute one for the other.
#[derive(Clone, Copy)]
pub struct F(f64);

const CANON_NAN: u64 = 0x7ff8_0000_0000_0000;

impl F {
    #[inline]
    pub fn new(x: f64) -> F {
        F(x)
    }

    /// The wrapped value, exactly as it was given.
    #[inline]
    pub fn get(self) -> f64 {
        self.0
    }

    #[inline]
    fn bits(self) -> u64 {
        if self.0.is_nan() {
            CANON_NAN
        } else {
            self.0.to_bits()
        }
    }

    /// True for `-0.0`, which prints and matches differently from `0.0`.
    #[inline]
    pub fn is_negative_zero(self) -> bool {
        self.0 == 0.0 && self.0.is_sign_negative()
    }

    /// Total order key: maps IEEE bits to a `u64` whose unsigned order matches
    /// the numeric order of the float (the standard `totalOrder` trick).
    #[inline]
    fn order_key(self) -> u64 {
        let b = self.bits();
        if b & (1 << 63) != 0 {
            !b
        } else {
            b ^ (1 << 63)
        }
    }
}

impl PartialEq for F {
    #[inline]
    fn eq(&self, other: &F) -> bool {
        self.bits() == other.bits()
    }
}
impl Eq for F {}

impl std::hash::Hash for F {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bits().hash(state)
    }
}

impl PartialOrd for F {
    #[inline]
    fn partial_cmp(&self, other: &F) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for F {
    #[inline]
    fn cmp(&self, other: &F) -> std::cmp::Ordering {
        self.order_key().cmp(&other.order_key())
    }
}

impl From<f64> for F {
    fn from(x: f64) -> F {
        F::new(x)
    }
}

impl fmt::Display for F {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let x = self.0;
        if x.is_nan() {
            f.write_str("NaN")
        } else if x.is_infinite() {
            f.write_str(if x > 0.0 { "inf" } else { "-inf" })
        } else if x == 0.0 && x.is_sign_negative() {
            // Printing `0` here would lose the sign, and `-0.0` is a different
            // value: reparsing must give back what was printed.
            f.write_str("-0")
        } else if x == x.trunc() && x.abs() < 1e15 {
            // Print integral values without a trailing ".0" so that rules and
            // golden output read naturally: `2` rather than `2.0`.
            write!(f, "{}", x as i64)
        } else {
            let s = format!("{}", x);
            f.write_str(&s)
        }
    }
}

impl fmt::Debug for F {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
