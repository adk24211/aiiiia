//! A sound interval abstraction over IEEE-754 doubles.
//!
//! Rewrite rules over floating point need side conditions: `x / x -> 1` is
//! wrong when `x` may be zero, infinite, or NaN; `sqrt(x^2) -> x` needs
//! `x >= 0`. This module supplies the facts those conditions ask for.
//!
//! Every transfer function rounds *outward* by one ULP, so the concrete result
//! of an operation is always inside the abstract result. `NaN` is tracked as a
//! separate flag rather than a point in the interval, because NaN is unordered
//! and would otherwise force every interval to be the whole line.

use std::fmt;

/// `lo <= hi` bounds on a value, plus whether it may be NaN.
#[derive(Clone, Copy, Debug)]
pub struct Interval {
    pub lo: f64,
    pub hi: f64,
    /// The value may be NaN. When this is false, the bounds are meaningful.
    pub nan: bool,
}

/// Compare two bounds reflexively.
///
/// `Eq` requires reflexivity, and the analysis fixpoint decides it has
/// converged by comparing facts. A bound that compared unequal to itself would
/// make that loop run forever, so a NaN bound -- which the transfer functions
/// try hard never to produce, but which no type forbids -- must still equal
/// itself here.
#[inline]
fn same_bound(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

impl PartialEq for Interval {
    fn eq(&self, other: &Interval) -> bool {
        same_bound(self.lo, other.lo) && same_bound(self.hi, other.hi) && self.nan == other.nan
    }
}

impl Eq for Interval {}

#[inline]
fn widen_lo(x: f64) -> f64 {
    if x.is_finite() {
        x.next_down()
    } else {
        x
    }
}

#[inline]
fn widen_hi(x: f64) -> f64 {
    if x.is_finite() {
        x.next_up()
    } else {
        x
    }
}

impl Interval {
    /// No information: any double, including NaN.
    pub const TOP: Interval = Interval {
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
        nan: true,
    };

    /// Any real value, but definitely not NaN.
    pub const REAL: Interval = Interval {
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
        nan: false,
    };

    pub fn new(lo: f64, hi: f64) -> Interval {
        debug_assert!(!lo.is_nan() && !hi.is_nan());
        Interval {
            lo: lo.min(hi),
            hi: hi.max(lo),
            nan: false,
        }
    }

    /// The abstraction of a single concrete value.
    pub fn point(x: f64) -> Interval {
        if x.is_nan() {
            Interval {
                lo: f64::INFINITY,
                hi: f64::NEG_INFINITY,
                nan: true,
            }
        } else {
            Interval {
                lo: x,
                hi: x,
                nan: false,
            }
        }
    }

    /// True when this interval describes no ordinary value (only possibly NaN).
    pub fn is_bottom_reals(&self) -> bool {
        self.lo > self.hi
    }

    fn with_nan(mut self, nan: bool) -> Interval {
        self.nan = nan;
        self
    }

    // -- predicates ---------------------------------------------------------

    /// Provably finite: not an infinity, not NaN.
    pub fn is_finite(&self) -> bool {
        !self.nan && self.lo.is_finite() && self.hi.is_finite()
    }
    /// Provably not zero (and not NaN).
    pub fn is_nonzero(&self) -> bool {
        !self.nan && (self.lo > 0.0 || self.hi < 0.0)
    }
    pub fn is_positive(&self) -> bool {
        !self.nan && self.lo > 0.0
    }
    pub fn is_negative(&self) -> bool {
        !self.nan && self.hi < 0.0
    }
    pub fn is_nonneg(&self) -> bool {
        !self.nan && self.lo >= 0.0
    }
    pub fn is_nonpos(&self) -> bool {
        !self.nan && self.hi <= 0.0
    }
    pub fn is_zero(&self) -> bool {
        !self.nan && self.lo == 0.0 && self.hi == 0.0
    }
    /// Provably finite and non-zero — the condition `x / x -> 1` needs.
    pub fn is_finite_nonzero(&self) -> bool {
        self.is_finite() && self.is_nonzero()
    }
    /// Provably never NaN.
    pub fn is_not_nan(&self) -> bool {
        !self.nan
    }
    /// The single value this interval pins down, if any.
    pub fn as_constant(&self) -> Option<f64> {
        if !self.nan && self.lo == self.hi && self.lo.is_finite() {
            Some(self.lo)
        } else {
            None
        }
    }

    // -- lattice ------------------------------------------------------------

    /// Combine two sound descriptions of the *same* value: the tighter of each
    /// bound, and NaN only if both allow it.
    pub fn meet(self, other: Interval) -> Interval {
        let lo = self.lo.max(other.lo);
        let hi = self.hi.min(other.hi);
        let nan = self.nan && other.nan;
        if lo > hi {
            // The two facts disagree about the reals. This means one of them is
            // unsound; keep the looser so the analysis stays conservative
            // rather than silently claiming an impossible range.
            return Interval {
                lo: self.lo.min(other.lo),
                hi: self.hi.max(other.hi),
                nan,
            };
        }
        Interval { lo, hi, nan }
    }

    /// Cover both possibilities — used where a value is one of two things.
    pub fn join(self, other: Interval) -> Interval {
        Interval {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
            nan: self.nan || other.nan,
        }
    }

    // -- transfer functions -------------------------------------------------
    //
    // These are named after the operators they abstract, not after the
    // `std::ops` traits they resemble. Implementing `Add` for an interval
    // would let `a + b` read as concrete arithmetic on a numeric type, which
    // is exactly the confusion to avoid: these compute a *range* that the
    // concrete result is guaranteed to fall inside.

    #[allow(clippy::should_implement_trait)]
    pub fn add(self, o: Interval) -> Interval {
        if self.is_bottom_reals() || o.is_bottom_reals() {
            return Interval::TOP;
        }
        // inf + (-inf) is NaN.
        let nan = self.nan
            || o.nan
            || (self.lo == f64::NEG_INFINITY && o.hi == f64::INFINITY)
            || (self.hi == f64::INFINITY && o.lo == f64::NEG_INFINITY);
        // A bound can itself come out NaN when the two infinities meet at that
        // corner. The NaN is recorded in the flag; the bound must fall back to
        // the unbounded side, because the *other* corners can still produce
        // ordinary reals and those have to stay inside the interval.
        let lo = widen_lo(self.lo + o.lo);
        let hi = widen_hi(self.hi + o.hi);
        Interval {
            lo: if lo.is_nan() { f64::NEG_INFINITY } else { lo },
            hi: if hi.is_nan() { f64::INFINITY } else { hi },
            nan,
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn sub(self, o: Interval) -> Interval {
        self.add(o.neg())
    }

    #[allow(clippy::should_implement_trait)]
    pub fn neg(self) -> Interval {
        if self.is_bottom_reals() {
            return Interval::TOP.with_nan(self.nan);
        }
        Interval {
            lo: -self.hi,
            hi: -self.lo,
            nan: self.nan,
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, o: Interval) -> Interval {
        if self.is_bottom_reals() || o.is_bottom_reals() {
            return Interval::TOP;
        }
        // 0 * inf is NaN.
        let zero_times_inf = |a: &Interval, b: &Interval| {
            a.lo <= 0.0 && a.hi >= 0.0 && (b.lo == f64::NEG_INFINITY || b.hi == f64::INFINITY)
        };
        let nan = self.nan || o.nan || zero_times_inf(&self, &o) || zero_times_inf(&o, &self);
        let prods = [
            self.lo * o.lo,
            self.lo * o.hi,
            self.hi * o.lo,
            self.hi * o.hi,
        ];
        // Any NaN among the corner products came from 0 * inf, already flagged.
        let clean: Vec<f64> = prods.iter().copied().filter(|p| !p.is_nan()).collect();
        if clean.is_empty() {
            return Interval::TOP;
        }
        let lo = clean.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = clean.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Interval {
            lo: widen_lo(lo),
            hi: widen_hi(hi),
            nan,
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn div(self, o: Interval) -> Interval {
        if self.is_bottom_reals() || o.is_bottom_reals() {
            return Interval::TOP;
        }
        // 0/0 and inf/inf are NaN; x/0 is an infinity.
        let straddles_zero = o.lo <= 0.0 && o.hi >= 0.0;
        let both_inf = (self.lo == f64::NEG_INFINITY || self.hi == f64::INFINITY)
            && (o.lo == f64::NEG_INFINITY || o.hi == f64::INFINITY);
        let nan =
            self.nan || o.nan || both_inf || (straddles_zero && self.lo <= 0.0 && self.hi >= 0.0);
        if straddles_zero {
            // The quotient is unbounded in at least one direction.
            return Interval::TOP.with_nan(nan);
        }
        let qs = [
            self.lo / o.lo,
            self.lo / o.hi,
            self.hi / o.lo,
            self.hi / o.hi,
        ];
        let clean: Vec<f64> = qs.iter().copied().filter(|q| !q.is_nan()).collect();
        if clean.is_empty() {
            return Interval::TOP;
        }
        let lo = clean.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = clean.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Interval {
            lo: widen_lo(lo),
            hi: widen_hi(hi),
            nan,
        }
    }

    pub fn abs(self) -> Interval {
        if self.is_bottom_reals() {
            return Interval::TOP.with_nan(self.nan);
        }
        let lo = if self.lo <= 0.0 && self.hi >= 0.0 {
            0.0
        } else {
            self.lo.abs().min(self.hi.abs())
        };
        let hi = self.lo.abs().max(self.hi.abs());
        Interval {
            lo,
            hi,
            nan: self.nan,
        }
    }

    pub fn sqrt(self) -> Interval {
        if self.is_bottom_reals() {
            return Interval::TOP;
        }
        // Negative inputs produce NaN.
        let nan = self.nan || self.lo < 0.0;
        let lo = self.lo.max(0.0).sqrt();
        let hi = self.hi.max(0.0).sqrt();
        Interval {
            lo: widen_lo(lo).max(0.0),
            hi: widen_hi(hi),
            nan,
        }
    }

    pub fn exp(self) -> Interval {
        if self.is_bottom_reals() {
            return Interval::TOP;
        }
        Interval {
            lo: widen_lo(self.lo.exp()).max(0.0),
            hi: widen_hi(self.hi.exp()),
            nan: self.nan,
        }
    }

    pub fn ln(self) -> Interval {
        if self.is_bottom_reals() {
            return Interval::TOP;
        }
        let nan = self.nan || self.lo < 0.0;
        let lo = if self.lo <= 0.0 {
            f64::NEG_INFINITY
        } else {
            widen_lo(self.lo.ln())
        };
        let hi = if self.hi <= 0.0 {
            f64::NEG_INFINITY
        } else {
            widen_hi(self.hi.ln())
        };
        Interval { lo, hi, nan }
    }

    /// `sin` and `cos` land in `[-1, 1]`, and are NaN exactly on non-finite
    /// input. Tighter bounds would need quadrant analysis; this is enough for
    /// the rules that consume it.
    pub fn bounded_trig(self) -> Interval {
        let nan = self.nan || !self.lo.is_finite() || !self.hi.is_finite();
        Interval {
            lo: -1.0,
            hi: 1.0,
            nan,
        }
    }

    pub fn min(self, o: Interval) -> Interval {
        // f64::min propagates a non-NaN operand, so the result is NaN only if
        // both can be.
        Interval {
            lo: self.lo.min(o.lo),
            hi: self.hi.min(o.hi),
            nan: self.nan && o.nan,
        }
    }

    pub fn max(self, o: Interval) -> Interval {
        Interval {
            lo: self.lo.max(o.lo),
            hi: self.hi.max(o.hi),
            nan: self.nan && o.nan,
        }
    }

    pub fn floor(self) -> Interval {
        Interval {
            lo: self.lo.floor(),
            hi: self.hi.floor(),
            nan: self.nan,
        }
    }

    pub fn ceil(self) -> Interval {
        Interval {
            lo: self.lo.ceil(),
            hi: self.hi.ceil(),
            nan: self.nan,
        }
    }

    pub fn sign(self) -> Interval {
        Interval {
            lo: if self.lo > 0.0 {
                1.0
            } else if self.lo < 0.0 {
                -1.0
            } else {
                0.0
            },
            hi: if self.hi > 0.0 {
                1.0
            } else if self.hi < 0.0 {
                -1.0
            } else {
                0.0
            },
            nan: self.nan,
        }
    }

    /// `pow` is only handled precisely for a constant, integral exponent;
    /// everything else falls back to a very weak but sound answer.
    pub fn pow(self, o: Interval) -> Interval {
        let Some(e) = o.as_constant() else {
            return Interval::TOP;
        };
        if e == 0.0 {
            // x^0 is 1 for every x, including NaN and infinities.
            return Interval::point(1.0);
        }
        if e == 1.0 {
            return self;
        }
        if e == e.trunc() && e.abs() <= 64.0 {
            let even = (e as i64) % 2 == 0;
            if e > 0.0 {
                let base = if even { self.abs() } else { self };
                let mut acc = base;
                for _ in 1..(e as i64) {
                    acc = acc.mul(base);
                }
                return Interval {
                    nan: self.nan,
                    ..acc
                };
            }
            let mut acc = self;
            for _ in 1..(-e as i64) {
                acc = acc.mul(self);
            }
            let r = Interval::point(1.0).div(if even { acc.abs() } else { acc });
            return Interval {
                nan: self.nan || r.nan,
                ..r
            };
        }
        // Non-integral exponents are NaN on negative bases.
        if self.is_nonneg() {
            return Interval {
                lo: 0.0,
                hi: f64::INFINITY,
                nan: self.nan,
            };
        }
        Interval::TOP
    }

    /// Comparisons and logical connectives produce exactly `0.0` or `1.0`.
    pub const BOOL: Interval = Interval {
        lo: 0.0,
        hi: 1.0,
        nan: false,
    };
}

impl Default for Interval {
    fn default() -> Interval {
        Interval::TOP
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_bottom_reals() {
            return f.write_str(if self.nan { "{NaN}" } else { "{}" });
        }
        let body = if self.lo == self.hi {
            format!("{{{}}}", self.lo)
        } else {
            format!("[{}, {}]", self.lo, self.hi)
        };
        if self.nan {
            write!(f, "{} u {{NaN}}", body)
        } else {
            f.write_str(&body)
        }
    }
}
