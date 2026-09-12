//! Differential testing: does an optimized expression still compute the same
//! thing as the one it came from?
//!
//! The safe rule tier claims to preserve IEEE-754 results exactly. This is how
//! that claim is tested rather than merely stated. Inputs are drawn from a
//! deliberately hostile distribution — subnormals, infinities, exact zeros of
//! both signs, magnitudes spanning eighty orders of magnitude — because a
//! float optimizer tested only on values near 1.0 is not tested at all.
//!
//! Every run is seeded, so a disagreement replays exactly.

use crate::eval::{eval, Env, EvalError};
use crate::lang::RecExpr;
use crate::rng::Rng;
use crate::sym::Sym;
use std::fmt::Write as _;

/// One input on which the two expressions disagreed.
#[derive(Clone, Debug)]
pub struct Sample {
    pub bindings: Vec<(Sym, f64)>,
    pub left: f64,
    pub right: f64,
    pub rel_err: f64,
}

impl Sample {
    /// The bindings written so they can be pasted back into `saturn eval`.
    pub fn as_flags(&self) -> String {
        self.bindings
            .iter()
            .map(|(s, v)| format!("-D {}={:?}", s, v))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The outcome of comparing two expressions over many inputs.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub samples: usize,
    pub agreed: usize,
    pub disagreed: usize,
    /// Inputs where both sides produced NaN. Counted separately because two
    /// NaNs agreeing is very weak evidence of anything.
    pub both_nan: usize,
    /// Inputs where at least one side could not be evaluated at all.
    pub errors: usize,
    pub max_rel_err: f64,
    pub worst: Option<Sample>,
    pub tolerance: f64,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.disagreed == 0
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        if self.samples == 0 {
            return "no inputs were sampled\n".to_string();
        }
        let _ = writeln!(
            s,
            "{} of {} inputs agreed (tolerance {}), worst relative error {:.3e}",
            self.agreed, self.samples, self.tolerance, self.max_rel_err
        );
        if self.both_nan > 0 {
            let _ = writeln!(s, "{} of those were NaN on both sides", self.both_nan);
        }
        if self.errors > 0 {
            let _ = writeln!(s, "{} inputs could not be evaluated", self.errors);
        }
        if let Some(w) = &self.worst {
            let _ = writeln!(s, "worst disagreement at");
            for (name, v) in &w.bindings {
                let _ = writeln!(s, "  {} = {:?}", name, v);
            }
            let _ = writeln!(s, "  original  -> {:?}", w.left);
            let _ = writeln!(s, "  optimized -> {:?}", w.right);
            let _ = writeln!(s, "  replay with: saturn eval '<expr>' {}", w.as_flags());
        }
        s
    }
}

/// Configures a comparison run.
#[derive(Clone, Debug)]
pub struct Checker {
    pub samples: usize,
    pub seed: u64,
    /// Relative tolerance. Zero demands bit equality, which is what the safe
    /// rule tier claims.
    pub tolerance: f64,
    /// Draw infinities, subnormals, and huge magnitudes as well as ordinary
    /// values.
    pub wild: bool,
    /// Sample uniformly from this range instead of the usual distribution.
    ///
    /// Reassociation is algebraically correct and numerically wrong: when a
    /// sum cancels, `(a + b) + c` and `a + (b + c)` can differ by everything.
    /// Testing such a rule against inputs eighty orders of magnitude apart
    /// measures cancellation, not the rule. A narrow range of comparable
    /// magnitudes measures the rule.
    pub range: Option<(f64, f64)>,
    /// Skip inputs on which either side is not finite.
    ///
    /// The fast-math tier is *licensed* to turn a NaN into a number --
    /// `?x / ?x => 1` is the whole point of it -- so comparing it on those
    /// inputs measures the licence rather than the rules. What is still worth
    /// checking is that where both sides produce an ordinary number, they
    /// produce the same one.
    pub finite_only: bool,
}

impl Default for Checker {
    fn default() -> Checker {
        Checker {
            samples: 2_000,
            seed: 0x5A7,
            tolerance: 1e-9,
            wild: false,
            range: None,
            finite_only: false,
        }
    }
}

impl Checker {
    pub fn new() -> Checker {
        Checker::default()
    }
    pub fn with_samples(mut self, n: usize) -> Self {
        self.samples = n;
        self
    }
    pub fn with_seed(mut self, s: u64) -> Self {
        self.seed = s;
        self
    }
    pub fn with_tolerance(mut self, t: f64) -> Self {
        self.tolerance = t;
        self
    }
    pub fn with_wild(mut self, w: bool) -> Self {
        self.wild = w;
        self
    }
    pub fn with_range(mut self, lo: f64, hi: f64) -> Self {
        self.range = Some((lo, hi));
        self
    }
    pub fn with_finite_only(mut self, f: bool) -> Self {
        self.finite_only = f;
        self
    }

    /// Compare `a` and `b` over random inputs.
    pub fn compare(&self, a: &RecExpr, b: &RecExpr) -> Report {
        let mut vars: Vec<Sym> = a.vars();
        vars.extend(b.vars());
        vars.sort_by_key(|s| s.as_str());
        vars.dedup();

        let mut rng = Rng::seed(self.seed);
        let mut report = Report {
            tolerance: self.tolerance,
            ..Report::default()
        };

        // With no variables there is exactly one input, and sampling it
        // thousands of times would say nothing more than sampling it once.
        let rounds = if vars.is_empty() { 1 } else { self.samples };

        for _ in 0..rounds {
            let bindings: Vec<(Sym, f64)> = vars
                .iter()
                .map(|&v| {
                    let x = match self.range {
                        Some((lo, hi)) => rng.range(lo, hi),
                        None if self.wild => rng.float(),
                        None => rng.tame_float(),
                    };
                    (v, x)
                })
                .collect();
            let env: Env = bindings.iter().copied().collect();
            report.samples += 1;

            let (left, right) = match (eval(a, &env), eval(b, &env)) {
                (Ok(l), Ok(r)) => (l, r),
                (Err(EvalError::Unbound(_)), _) | (_, Err(EvalError::Unbound(_))) => {
                    // Cannot happen: every variable of both sides is bound.
                    report.errors += 1;
                    continue;
                }
                _ => {
                    // A surviving `d(...)` on either side. That is a failure of
                    // the rules, not a numeric disagreement, so it is counted
                    // apart from the tolerance check.
                    report.errors += 1;
                    continue;
                }
            };

            if self.finite_only && (!left.is_finite() || !right.is_finite()) {
                report.samples -= 1;
                continue;
            }

            let err = relative_error(left, right);
            if err > report.max_rel_err && err.is_finite() {
                report.max_rel_err = err;
            }
            if agree(left, right, self.tolerance) {
                report.agreed += 1;
                if left.is_nan() {
                    report.both_nan += 1;
                }
            } else {
                report.disagreed += 1;
                let worse = report
                    .worst
                    .as_ref()
                    .map(|w| err > w.rel_err || w.rel_err.is_nan())
                    .unwrap_or(true);
                if worse {
                    report.worst = Some(Sample {
                        bindings,
                        left,
                        right,
                        rel_err: err,
                    });
                }
            }
        }
        report
    }
}

/// Do `x` and `y` count as the same answer?
///
/// Bit equality first, so `tolerance = 0` means exactly that. Then the two
/// cases IEEE-754 makes special: NaN is never equal to itself, and two
/// infinities of the same sign have no meaningful relative error.
pub fn agree(x: f64, y: f64, tolerance: f64) -> bool {
    if x.to_bits() == y.to_bits() {
        return true;
    }
    if x.is_nan() || y.is_nan() {
        // One side NaN and the other not is exactly the bug this is for.
        return x.is_nan() && y.is_nan();
    }
    if x.is_infinite() || y.is_infinite() {
        return x == y;
    }
    // `x == y` still has to be checked because +0.0 and -0.0 have different
    // bits but are the same number, and an optimizer is allowed to move
    // between them wherever nothing can observe the sign.
    if x == y {
        return true;
    }
    (x - y).abs() <= tolerance * scale(x, y)
}

/// The magnitude a difference is measured against.
///
/// Dividing by `max(|x|, |y|)` alone would make the test impossibly strict
/// near zero, where an absolute difference of `1e-300` would read as a
/// relative error of 1. Flooring the scale at 1 turns the test into an
/// absolute one for small values and a relative one for large ones, which is
/// what "the same answer" means in practice.
fn scale(x: f64, y: f64) -> f64 {
    x.abs().max(y.abs()).max(1.0)
}

/// The relative difference between `x` and `y`, measured the same way
/// [`agree`] measures it. Zero when they are identical, infinite when exactly
/// one of them is NaN or an infinity.
pub fn relative_error(x: f64, y: f64) -> f64 {
    if x.to_bits() == y.to_bits() || x == y {
        return 0.0;
    }
    if x.is_nan() || y.is_nan() || x.is_infinite() || y.is_infinite() {
        return f64::INFINITY;
    }
    (x - y).abs() / scale(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn cmp(a: &str, b: &str, tol: f64) -> Report {
        Checker::new()
            .with_samples(500)
            .with_tolerance(tol)
            .with_wild(true)
            .compare(&parse(a).unwrap(), &parse(b).unwrap())
    }

    #[test]
    fn identical_expressions_agree() {
        let r = cmp("x * y + 1", "x * y + 1", 0.0);
        assert!(r.ok(), "{}", r.render());
        assert_eq!(r.agreed, r.samples);
        assert_eq!(r.max_rel_err, 0.0);
    }

    #[test]
    fn an_exact_identity_agrees_at_tolerance_zero() {
        let r = cmp("x * 1", "x", 0.0);
        assert!(r.ok(), "{}", r.render());
        let r = cmp("-(-x)", "x", 0.0);
        assert!(r.ok(), "{}", r.render());
    }

    #[test]
    fn an_inexact_identity_is_caught_at_tolerance_zero() {
        // Reassociation is not exact in floating point, and with inputs
        // spanning many orders of magnitude the difference shows up quickly.
        let r = cmp("(x + y) + z", "x + (y + z)", 0.0);
        assert!(!r.ok(), "reassociation went undetected: {}", r.render());
        assert!(r.worst.is_some());
    }

    #[test]
    fn a_one_sided_nan_is_a_disagreement() {
        // `x - x` is NaN at x = inf, but 0 everywhere else.
        let r = cmp("x - x", "0", 0.0);
        assert!(!r.ok(), "{}", r.render());
        let w = r.worst.expect("a worst sample");
        assert!(w.left.is_nan() || w.right.is_nan());
    }

    #[test]
    fn signed_zero_counts_as_agreement() {
        // Nothing downstream of a bare value can tell them apart, and every
        // rule that could is guarded separately.
        assert!(agree(0.0, -0.0, 0.0));
        assert_eq!(relative_error(0.0, -0.0), 0.0);
    }

    #[test]
    fn infinities_must_match_in_sign() {
        assert!(agree(f64::INFINITY, f64::INFINITY, 0.0));
        assert!(!agree(f64::INFINITY, f64::NEG_INFINITY, 1e9));
        assert!(!agree(f64::INFINITY, 1e308, 1e9));
        assert!(relative_error(f64::INFINITY, 1.0).is_infinite());
    }

    #[test]
    fn nan_agrees_only_with_nan() {
        assert!(agree(f64::NAN, f64::NAN, 0.0));
        assert!(!agree(f64::NAN, 0.0, 1e9));
        assert!(!agree(0.0, f64::NAN, 1e9));
    }

    #[test]
    fn the_scale_floor_keeps_tiny_values_from_failing() {
        // Without the floor at 1, these would read as a relative error of 1.
        assert!(agree(1e-300, 2e-300, 1e-9));
        // But the floor must not excuse a genuine difference at scale.
        assert!(!agree(1e10, 1.1e10, 1e-9));
    }

    #[test]
    fn a_constant_expression_is_sampled_once() {
        let r = cmp("2 * 3", "6", 0.0);
        assert_eq!(r.samples, 1);
        assert!(r.ok());
    }

    #[test]
    fn the_same_seed_replays_exactly() {
        let a = parse("sin(x) / y").unwrap();
        let b = parse("sin(x) * (1 / y)").unwrap();
        let c = Checker::new()
            .with_seed(42)
            .with_samples(200)
            .with_wild(true);
        let first = c.compare(&a, &b);
        let second = c.compare(&a, &b);
        assert_eq!(first.agreed, second.agreed);
        assert_eq!(first.disagreed, second.disagreed);
        assert_eq!(
            first.worst.as_ref().map(|w| w.as_flags()),
            second.worst.as_ref().map(|w| w.as_flags())
        );
    }

    #[test]
    fn a_different_seed_samples_different_inputs() {
        let a = parse("x + 1e300").unwrap();
        let b = parse("x").unwrap();
        let one = Checker::new()
            .with_seed(1)
            .with_samples(200)
            .compare(&a, &b);
        let two = Checker::new()
            .with_seed(2)
            .with_samples(200)
            .compare(&a, &b);
        assert_ne!(
            one.worst.as_ref().map(|w| w.as_flags()),
            two.worst.as_ref().map(|w| w.as_flags())
        );
    }

    #[test]
    fn an_unreduced_derivative_is_counted_not_panicked() {
        let a = parse("d(x, x * x)").unwrap();
        let b = parse("2 * x").unwrap();
        let r = Checker::new().with_samples(20).compare(&a, &b);
        assert_eq!(r.errors, r.samples);
        assert!(r.ok(), "errors are not numeric disagreements");
    }

    #[test]
    fn the_report_names_the_input_that_broke_it() {
        let r = cmp("x - x", "0", 0.0);
        let text = r.render();
        assert!(text.contains("worst disagreement"), "{}", text);
        assert!(text.contains("x = "), "{}", text);
        assert!(text.contains("saturn eval"), "{}", text);
    }
}
