//! A small deterministic PRNG.
//!
//! `saturn` has no dependencies, and the randomized tests need to be
//! reproducible anyway: a differential-testing failure is only useful if the
//! seed that produced it replays exactly. This is xoshiro256++, seeded through
//! SplitMix64.

/// xoshiro256++ — fast, well-distributed, and not cryptographic.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    pub fn seed(seed: u64) -> Rng {
        let mut x = seed;
        Rng {
            s: [
                splitmix64(&mut x),
                splitmix64(&mut x),
                splitmix64(&mut x),
                splitmix64(&mut x),
            ],
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[0]
            .wrapping_add(self.s[3])
            .rotate_left(23)
            .wrapping_add(self.s[0]);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)`, using the top 53 bits.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    pub fn bool(&mut self, p: f64) -> bool {
        self.next_f64() < p
    }

    /// A value drawn from a distribution that spans many orders of magnitude
    /// and occasionally lands on an awkward special value.
    ///
    /// Testing a float optimizer on inputs drawn from `[0, 1)` would miss
    /// almost everything that makes floating point hard, so this deliberately
    /// samples subnormals, huge magnitudes, exact zeros, and infinities.
    pub fn float(&mut self) -> f64 {
        match self.below(16) {
            0 => 0.0,
            1 => -0.0,
            2 => 1.0,
            3 => -1.0,
            4 => f64::INFINITY,
            5 => f64::NEG_INFINITY,
            6 => f64::MIN_POSITIVE * self.next_f64(),
            7 => f64::MAX * self.next_f64(),
            8..=11 => self.range(-10.0, 10.0),
            _ => {
                // Log-uniform over a wide range, with a random sign.
                let exp = self.range(-40.0, 40.0);
                let sign = if self.bool(0.5) { -1.0 } else { 1.0 };
                sign * 10f64.powf(exp)
            }
        }
    }

    /// Like [`Rng::float`], but restricted to ordinary finite values — what
    /// most equivalence checks want, since both sides agreeing on NaN says
    /// very little.
    pub fn tame_float(&mut self) -> f64 {
        match self.below(8) {
            0 => 0.0,
            1 => 1.0,
            2 => -1.0,
            3..=5 => self.range(-10.0, 10.0),
            _ => {
                let exp = self.range(-8.0, 8.0);
                let sign = if self.bool(0.5) { -1.0 } else { 1.0 };
                sign * 10f64.powf(exp)
            }
        }
    }
}

impl Default for Rng {
    fn default() -> Rng {
        Rng::seed(0x5A7_0000_0000)
    }
}
