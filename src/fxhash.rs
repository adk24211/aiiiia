//! A fast, non-cryptographic hasher for the engine's internal maps.
//!
//! The hashcons is looked up on the order of tens of millions of times per
//! saturating run, and its keys are e-nodes: an operator and up to three
//! 32-bit ids. The standard library's default is SipHash-1-3, chosen so that
//! a `HashMap` exposed to untrusted keys cannot be driven into its worst case.
//! Nothing here is exposed to untrusted keys — the keys are nodes the engine
//! built itself — so the trade is the wrong way round.
//!
//! This is rustc's own `FxHasher`: rotate, xor, multiply. It has no collision
//! resistance worth the name and must not be used where an attacker chooses
//! the keys.

use std::hash::{BuildHasherDefault, Hasher};

/// Use as `HashMap<K, V, FxBuildHasher>`.
pub type FxBuildHasher = BuildHasherDefault<FxHasher>;

/// A `HashMap` using [`FxHasher`].
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;

/// A `HashSet` using [`FxHasher`].
pub type FxHashSet<T> = std::collections::HashSet<T, FxBuildHasher>;

/// The multiplier from rustc's `FxHasher`: an odd 64-bit constant with
/// well-distributed bits, so a multiply mixes the low bits upward.
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while rest.len() >= 8 {
            let (word, tail) = rest.split_at(8);
            self.add(u64::from_ne_bytes(word.try_into().expect("eight bytes")));
            rest = tail;
        }
        if rest.len() >= 4 {
            let (word, tail) = rest.split_at(4);
            self.add(u32::from_ne_bytes(word.try_into().expect("four bytes")) as u64);
            rest = tail;
        }
        for &b in rest {
            self.add(b as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(n as u64);
    }
    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn hash_of<T: Hash>(x: &T) -> u64 {
        let mut h = FxHasher::default();
        x.hash(&mut h);
        h.finish()
    }

    #[test]
    fn equal_values_hash_equally() {
        assert_eq!(hash_of(&(1u32, 2u32, 3u32)), hash_of(&(1u32, 2u32, 3u32)));
        assert_eq!(hash_of(&"saturn"), hash_of(&"saturn"));
    }

    #[test]
    fn small_differences_spread() {
        // Not a quality claim, just that consecutive keys do not collide --
        // which a plain sum or xor of the words would.
        let hashes: FxHashSet<u64> = (0u32..4096).map(|i| hash_of(&(i, i + 1))).collect();
        assert_eq!(hashes.len(), 4096);
        let ids: FxHashSet<u64> = (0u32..4096).map(|i| hash_of(&i)).collect();
        assert_eq!(ids.len(), 4096);
    }

    #[test]
    fn a_map_behaves_like_a_map() {
        let mut m: FxHashMap<(u32, u32), u32> = FxHashMap::default();
        for i in 0..1000u32 {
            m.insert((i, i * 7), i);
        }
        for i in 0..1000u32 {
            assert_eq!(m.get(&(i, i * 7)), Some(&i));
        }
        assert_eq!(m.get(&(1, 1)), None);
        assert_eq!(m.len(), 1000);
    }
}
