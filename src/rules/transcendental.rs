//! Powers, roots, exponentials, logarithms, and trigonometry.
//!
//! Almost nothing in here is exact. C imposes no accuracy requirement on
//! `pow`, `exp`, `ln` or the trig functions, so a rule that trades one
//! arrangement of them for another moves the last bits of the result even
//! when it is perfect over the reals. The safe tier is correspondingly thin:
//! it holds the special cases IEEE-754 and C99 pin down exactly — `pow(x, ±0)`
//! is 1 for every `x`, NaN included — plus the parity of the trig functions,
//! which follows from how they reduce their argument rather than from how
//! accurately they compute.
//!
//! Everything with real mathematical content — `ln(exp(x))`, the Pythagorean
//! identity, the double-angle formulas — is in the fast tier.
//!
//! The rules for literal arguments (`exp(0) => 1` and friends) look like
//! constant folding, which the analysis already does. They earn their place
//! because folding of the transcendental operators is switchable: a build
//! that must reproduce the runtime's bits exactly sets
//! [`MathAnalysis::fold_transcendental`](crate::MathAnalysis) to false, and
//! these are the identities that survive it.
//!
//! One property of the fast tier is worth stating outright. An e-graph union
//! is global, so a domain assumption implied by one subterm leaks into every
//! other use of the same class: saturating `x ^ 0.5 * x ^ 0.5` with both tiers
//! ends up proving `x = abs(x)`, which is sound only because writing
//! `x ^ 0.5` at all assumed `x >= 0`. That is the bargain `-ffast-math`
//! strikes everywhere; equality saturation just makes it visible.

use super::{and, is_int_const, is_nonneg, is_not_nan, is_positive, Rule};
use crate::rw;

/// Identities that hold bit for bit on every double, including the
/// infinities, both zeros, and NaN.
pub fn safe() -> Vec<Rule> {
    vec![
        // `pow(x, ±0)` is 1 for every x. This is the one corner of the
        // language where a NaN operand does not poison the result, so the
        // rule needs no guard.
        rw!("pow-zero"; "?x ^ 0" => "1"),
        // `pow(x, 1)` is x itself, sign of zero included.
        rw!("pow-one"; "?x ^ 1" => "?x"),
        // `pow(1, y)` is 1 for every y, again including NaN.
        rw!("one-pow"; "1 ^ ?x" => "1"),
        // `pow(+0, y)` is +0 only above zero: at y = 0 it is 1, below it is an
        // infinity, and at NaN it is NaN. The base also matches a written
        // `-0.0` — see `sin-zero` below — but harmlessly, because the
        // right-hand side is that very e-class rather than a fresh `+0`.
        rw!("zero-pow"; "0 ^ ?x" => "0", if "?x > 0", is_positive("?x")),
        // C99 fixes all three exactly: exp(±0) is 1, log(1) is +0, cos(±0) is 1.
        rw!("exp-zero"; "exp(0)" => "1"),
        rw!("ln-one"; "ln(1)" => "0"),
        rw!("cos-zero"; "cos(0)" => "1"),
        // `sin` and `tan` return their argument at zero, so they carry the
        // sign of the zero through, and a rule naming `+0` on both sides would
        // have to justify the -0.0 case. It does not arise: [`crate::sym::F`]
        // hashes -0.0 and +0.0 alike, so the two are one e-class before any
        // rule looks at them, and the right-hand side here is that same class.
        rw!("sin-zero"; "sin(0)" => "0"),
        rw!("tan-zero"; "tan(0)" => "0"),
        // cos of the double nearest pi is -1 + 7.5e-33, and the next double
        // along from -1 is 1.1e-16 away, so anything short of a catastrophically
        // wrong cos returns exactly -1;
        // `special_values_hold_on_this_platform` checks the claim here.
        // sin of that same double is nowhere near zero, which is why it gets
        // no companion rule in either tier — see [`fast_math`].
        rw!("cos-pi"; "cos(pi)" => "-1"),
        // Parity is exact rather than approximate: argument reduction works on
        // the magnitude and re-applies the sign afterwards, so the two sides
        // run the same code on the same bits. `trig_parity_is_exact` holds the
        // platform to it.
        rw!("sin-neg"; "sin(-?x)" => "-sin(?x)"),
        rw!("cos-neg"; "cos(-?x)" => "cos(?x)"),
        rw!("tan-neg"; "tan(-?x)" => "-tan(?x)"),
        // `atan2(±0, x)` is ±0 for positive x, and `atan2(y, ±0)` is +pi/2 for
        // positive y. Halving is exact, so `pi / 2` folds to precisely the
        // double the libm returns.
        rw!("atan2-zero-num"; "atan2(0, ?x)" => "0", if "?x > 0", is_positive("?x")),
        rw!("atan2-zero-den"; "atan2(?y, 0)" => "pi / 2", if "?y > 0", is_positive("?y")),
        // atan2 lands in [-pi, pi], so clamping it there is a no-op — but only
        // once it is known not to be NaN, because `min` and `max` return the
        // *other* operand when one side is NaN and would otherwise turn a NaN
        // into pi. atan2 is NaN exactly when an argument is.
        rw!("atan2-clamp-hi"; "min(atan2(?y, ?x), pi)" => "atan2(?y, ?x)",
            if "neither argument is NaN", and(is_not_nan("?y"), is_not_nan("?x"))),
        rw!("atan2-clamp-lo"; "max(atan2(?y, ?x), -pi)" => "atan2(?y, ?x)",
            if "neither argument is NaN", and(is_not_nan("?y"), is_not_nan("?x"))),
    ]
}

/// Identities that hold over the reals but not over the floats.
pub fn fast_math() -> Vec<Rule> {
    vec![
        // -- powers ---------------------------------------------------------
        //
        // Correctly rounded, `pow(x, 2)` is `x * x` and `pow(x, -1)` is
        // `1 / x`; glibc does exactly that. Nothing requires it to, and a
        // multiply is twenty times cheaper than a `pow`, so the trade belongs
        // in the tier that admits it is a trade.
        rw!("pow-square"; "?x ^ 2" => "?x * ?x"),
        rw!("pow-recip"; "?x ^ -1" => "1 / ?x"),
        // `pow` and `sqrt` disagree on two inputs whatever the accuracy:
        // `pow(-0, 0.5)` is +0 where `sqrt(-0)` is -0, and `pow(-inf, 0.5)` is
        // +inf where `sqrt(-inf)` is NaN.
        //
        // Only this direction. Turning every `sqrt` back into a `pow` would
        // let `pow-add` rediscover `sqrt(?x) * sqrt(?x) => ?x` below without
        // the non-negativity check that rule carries.
        rw!("pow-half"; "?x ^ 0.5" => "sqrt(?x)"),
        // True over the reals wherever both sides are defined. Over floats,
        // `0^1 * 0^-1` is `0 * inf`, which is NaN, while `0^0` is 1.
        rw!("pow-add"; "?x ^ ?a * ?x ^ ?b" => "?x ^ (?a + ?b)"),
        rw!("pow-sub"; "?x ^ ?a / ?x ^ ?b" => "?x ^ (?a - ?b)"),
        // `(x^a)^b = x^(ab)` is false over the reals for a negative base:
        // `((-1)^2)^0.5` is 1 but `(-1)^1` is -1. Both sides are defined
        // there, so this needs a real condition, not just a domain excuse —
        // either a positive base, or integer exponents throughout.
        rw!("pow-pow-pos"; "(?x ^ ?a) ^ ?b" => "?x ^ (?a * ?b)",
            if "?x > 0", is_positive("?x")),
        rw!("pow-pow-int"; "(?x ^ ?a) ^ ?b" => "?x ^ (?a * ?b)",
            if "?a and ?b are integers", and(is_int_const("?a"), is_int_const("?b"))),
        // Splitting doubles the rounding error and the `pow` count; joining
        // halves both, which is why both directions are here. Neither holds
        // over the reals either: `((-1) * (-1)) ^ 0.5` is 1 while
        // `(-1) ^ 0.5 * (-1) ^ 0.5` is NaN, so the split form can invent a
        // domain error the joined one never had.
        rw!("pow-prod"; "(?x * ?y) ^ ?a" => "?x ^ ?a * ?y ^ ?a"),
        rw!("pow-prod-join"; "?x ^ ?a * ?y ^ ?a" => "(?x * ?y) ^ ?a"),
        // -- roots ----------------------------------------------------------
        //
        // NaN below zero, and two roundings above it.
        rw!("sqrt-square"; "sqrt(?x) * sqrt(?x)" => "?x", if "?x >= 0", is_nonneg("?x")),
        // `sqrt(x * x)` is exactly `abs(x)` — but only while `x * x` is both
        // finite and normal. Above 1.3e154 the square overflows and the left
        // side is inf where `abs` is finite; below 1.5e-154 it lands in the
        // subnormals, where squaring has already thrown away the low bits that
        // the root would need to get back. `abs` is right in every case, which
        // is what makes this worth having at all.
        rw!("sqrt-of-square"; "sqrt(?x * ?x)" => "abs(?x)"),
        // -- exp and ln -----------------------------------------------------
        //
        // True for every real x, and wrong at both ends in floats: `exp`
        // overflows to inf above 710 and flushes to zero below -746, and
        // `ln` of either is not x.
        rw!("ln-exp"; "ln(exp(?x))" => "?x"),
        // The other direction is not a rounding question at all — `ln` of a
        // negative is NaN — so the domain check stays even in the fast tier.
        rw!("exp-ln"; "exp(ln(?x))" => "?x", if "?x > 0", is_positive("?x")),
        // Joining is the profitable direction: one `exp` instead of two, and
        // an add instead of a multiply.
        rw!("exp-prod"; "exp(?a) * exp(?b)" => "exp(?a + ?b)"),
        rw!("exp-quot"; "exp(?a) / exp(?b)" => "exp(?a - ?b)"),
        // Reciprocal and negated exponent differ by a rounding about a quarter
        // of the time over [-20, 20]. The second direction is what lets
        // `exp(x) * exp(-x)` find its way to 1.
        rw!("exp-neg"; "exp(-?x)" => "1 / exp(?x)"),
        rw!("exp-recip"; "1 / exp(?x)" => "exp(-?x)"),
        // Splitting a log is only vacuously true where it fails: `ln(-2 * -3)`
        // is an ordinary number while `ln(-2) + ln(-3)` is NaN, and the reals
        // never had the right-hand side to begin with. Both directions are
        // worth their place because which one pays depends on the context:
        // joining drops a `ln`, splitting can expose a `ln(exp(_))` to cancel.
        rw!("ln-prod"; "ln(?x * ?y)" => "ln(?x) + ln(?y)"),
        rw!("ln-prod-join"; "ln(?x) + ln(?y)" => "ln(?x * ?y)"),
        rw!("ln-quot"; "ln(?x / ?y)" => "ln(?x) - ln(?y)"),
        rw!("ln-quot-join"; "ln(?x) - ln(?y)" => "ln(?x / ?y)"),
        // Only this direction. The reverse, `?k * ln(?x) => ln(?x ^ ?k)`,
        // matches every product with a logarithm anywhere in it and builds a
        // term dearer than the one it came from, so it would grow the graph
        // for nothing.
        rw!("ln-pow"; "ln(?x ^ ?k)" => "?k * ln(?x)"),
        // -- trigonometry ---------------------------------------------------
        //
        // `sin(x)^2 + cos(x)^2` is not 1 in floats, and is NaN rather than 1
        // at the infinities. The reverse direction is deliberately absent: `1`
        // appears in nearly every expression, and rewriting it to
        // `sin(?x)^2 + cos(?x)^2` for every `?x` in the graph never stops.
        rw!("pythagoras"; "sin(?x) ^ 2 + cos(?x) ^ 2" => "1"),
        rw!("pythagoras-mul"; "sin(?x) * sin(?x) + cos(?x) * cos(?x)" => "1"),
        // These two are the rearrangements of the identity, not its reverse:
        // each has a left-hand side to match on, so neither runs away.
        rw!("cos-square"; "1 - sin(?x) ^ 2" => "cos(?x) ^ 2"),
        rw!("sin-square"; "1 - cos(?x) ^ 2" => "sin(?x) ^ 2"),
        // A divide and a second transcendental against one `tan`.
        rw!("tan-def"; "tan(?x)" => "sin(?x) / cos(?x)"),
        rw!("tan-join"; "sin(?x) / cos(?x)" => "tan(?x)"),
        rw!("sin-double"; "sin(2 * ?x)" => "2 * sin(?x) * cos(?x)"),
        rw!("sin-double-join"; "2 * sin(?x) * cos(?x)" => "sin(2 * ?x)"),
        rw!("cos-double"; "cos(2 * ?x)" => "cos(?x) ^ 2 - sin(?x) ^ 2"),
        rw!("cos-double-join"; "cos(?x) ^ 2 - sin(?x) ^ 2" => "cos(2 * ?x)"),
        // There is deliberately no `sin(pi) => 0` in either tier. `pi` is the
        // double nearest pi, its sine is 1.2246467991473532e-16, and the
        // analysis folds it to exactly that. A rule saying the same class is
        // also 0 does not merely lose precision: it hands `MathAnalysis::merge`
        // two different literals for one value, which is the shape of an
        // unsound rule and trips its debug assertion. `cos(pi)` is in the safe
        // tier instead, because there the folded value really is -1.
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::MathAnalysis;
    use crate::eval::{eval, Env};
    use crate::extract::{Extractor, OpCost};
    use crate::lang::RecExpr;
    use crate::parser::parse;
    use crate::rng::Rng;
    use crate::runner::Runner;
    use crate::sym::Sym;

    /// NaN compares unequal to itself, so a bitwise check needs to say what it
    /// means by "the same NaN" — any of them, since the language has only one.
    fn same_bits(a: f64, b: f64) -> bool {
        (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
    }

    fn optimize(src: &str, rules: &[Rule]) -> RecExpr {
        optimize_with(src, rules, true)
    }

    fn optimize_with(src: &str, rules: &[Rule], fold_transcendental: bool) -> RecExpr {
        let expr = parse(src).unwrap();
        let runner = Runner::new(MathAnalysis {
            fold_transcendental,
            ..MathAnalysis::default()
        })
        .with_iter_limit(15)
        .with_node_limit(20_000)
        .with_expr(&expr)
        .run(rules);
        let (_, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
        best
    }

    /// True when saturating `src` puts `other` in the root's e-class.
    ///
    /// Extraction can only reveal a rewrite that made the term cheaper, so it
    /// says nothing about `sin(-?x) => -sin(?x)`, whose two sides cost the
    /// same. This asks the e-graph directly whether the equality was proven.
    fn proves_equal(src: &str, other: &str, rules: &[Rule]) -> bool {
        let expr = parse(src).unwrap();
        let mut runner = Runner::new(MathAnalysis::default())
            .with_iter_limit(15)
            .with_node_limit(20_000)
            .with_expr(&expr)
            .run(rules);
        let root = runner.root();
        let other = runner.egraph.add_expr(&parse(other).unwrap());
        runner.egraph.find(other) == runner.egraph.find(root)
    }

    /// Every double that makes a rule in this module interesting, plus two
    /// unremarkable ones so the grid is not all corner cases.
    const HARD: [f64; 14] = [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -0.0,
        1.0,
        -1.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        f64::MIN,
        2.5,
        -3.25,
    ];

    #[test]
    fn rule_names_are_unique() {
        let mut names: Vec<String> = safe()
            .iter()
            .chain(fast_math().iter())
            .map(|r| r.name.clone())
            .collect();
        let total = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate rule name");
    }

    #[test]
    fn special_values_hold_on_this_platform() {
        let (nan, inf, pi) = (f64::NAN, f64::INFINITY, std::f64::consts::PI);
        assert_eq!(nan.powf(0.0), 1.0);
        assert_eq!(inf.powf(0.0), 1.0);
        assert_eq!(1.0f64.powf(nan), 1.0);
        assert!(nan.powf(1.0).is_nan());
        assert!(same_bits((-0.0f64).powf(1.0), -0.0));
        assert!(same_bits(0.0f64.powf(3.0), 0.0));
        assert_eq!(0.0f64.exp(), 1.0);
        assert!(same_bits(1.0f64.ln(), 0.0));
        assert!(same_bits(0.0f64.sin(), 0.0));
        assert_eq!(0.0f64.cos(), 1.0);
        assert!(same_bits(0.0f64.tan(), 0.0));
        assert_eq!(pi.cos(), -1.0);
        assert!(same_bits(0.0f64.atan2(1.0), 0.0));
        assert_eq!(1.0f64.atan2(0.0), pi / 2.0);
        assert_eq!(1.0f64.atan2(-0.0), pi / 2.0);
        assert_eq!(inf.atan2(0.0), pi / 2.0);
        // What the clamp rules' side conditions exist for: `min`/`max` drop a
        // NaN rather than propagating it, so an unguarded clamp would turn a
        // NaN atan2 into ±pi.
        assert_eq!(nan.min(pi), pi);
        assert_eq!(nan.max(-pi), -pi);
    }

    #[test]
    fn trig_parity_rules_prove_the_equality() {
        let rules = safe();
        assert_eq!(optimize("cos(-x)", &rules).pretty(), "cos(x)");
        // `sin(-x)` and `-sin(x)` cost the same, so only the e-graph can say
        // whether the rule fired.
        assert!(proves_equal("sin(-x)", "-sin(x)", &rules));
        assert!(proves_equal("tan(-x)", "-tan(x)", &rules));
        // Nothing here claims the odd functions are even as well.
        assert!(!proves_equal("sin(-x)", "sin(x)", &rules));
        assert!(!proves_equal("tan(-x)", "tan(x)", &rules));
    }

    #[test]
    fn atan2_zero_rules_need_a_positive_other_argument() {
        let rules = safe();
        assert_eq!(
            optimize("atan2(0, max(min(x, 3), 1))", &rules).pretty(),
            "0"
        );
        let quarter_turn = optimize("atan2(max(min(x, 3), 1), 0)", &rules);
        assert_eq!(
            eval(&quarter_turn, &Env::new()).unwrap(),
            std::f64::consts::PI / 2.0
        );
        // A bare variable could be negative, zero, or NaN in either slot, and
        // atan2 returns something different in each of those cases.
        assert!(optimize("atan2(0, x)", &rules)
            .pretty()
            .starts_with("atan2"));
        assert!(optimize("atan2(x, 0)", &rules)
            .pretty()
            .starts_with("atan2"));
    }

    #[test]
    fn trig_parity_is_exact() {
        let mut rng = Rng::seed(0x5EED);
        for _ in 0..20_000 {
            let x = rng.float();
            assert!(same_bits((-x).sin(), -x.sin()), "sin is odd at {:e}", x);
            assert!(same_bits((-x).cos(), x.cos()), "cos is even at {:e}", x);
            assert!(same_bits((-x).tan(), -x.tan()), "tan is odd at {:e}", x);
        }
    }

    /// The claim the safe tier makes, checked the only way it can be: run the
    /// rules, extract, and compare the two expressions bit for bit.
    ///
    /// The inputs are the exhaustive product of [`HARD`] — so the infinities,
    /// both zeros, a subnormal and NaN are tried in every slot of every case,
    /// rather than turning up with whatever probability a sampler gives them —
    /// with a few random draws appended to cover ordinary magnitudes. Every
    /// rule in [`safe`] that can take a variable appears at least once; the
    /// clamps are what let the guarded rules fire at all.
    #[test]
    fn safe_rules_preserve_every_bit() {
        let rules = safe();
        let cases = [
            "x ^ 1",
            "x ^ 0",
            "1 ^ x",
            "0 ^ max(min(x, 3), 1)",
            "sin(-x)",
            "cos(-x)",
            "tan(-x)",
            "x ^ 1 + y ^ 0",
            "cos(-(x * y))",
            "sin(-x) * cos(-y)",
            "exp(0) * x",
            "ln(1) + x",
            "cos(0) * x",
            "sin(0) + x",
            "tan(0) + x",
            "cos(pi) * x",
            "atan2(0, 4) + y",
            "atan2(0, max(min(x, 3), 1))",
            "atan2(max(min(x, 3), 1), 0) * y",
            "min(atan2(max(min(y, 2), 1), max(min(x, 2), 1)), pi)",
            "max(atan2(max(min(y, 2), 1), max(min(x, 2), 1)), -pi)",
        ];
        let mut rng = Rng::seed(0xC0FFEE);
        let mut values = HARD.to_vec();
        values.extend((0..6).map(|_| rng.float()));

        for src in cases {
            let before = parse(src).unwrap();
            let after = optimize(src, &rules);
            for &x in &values {
                for &y in &values {
                    let mut env = Env::new();
                    env.insert(Sym::new("x"), x);
                    env.insert(Sym::new("y"), y);
                    let a = eval(&before, &env).unwrap();
                    let b = eval(&after, &env).unwrap();
                    assert!(
                        same_bits(a, b),
                        "`{}` became `{}`, which gives {:e} instead of {:e} at {:?}",
                        src,
                        after.pretty(),
                        b,
                        a,
                        env
                    );
                }
            }
        }
    }

    #[test]
    fn safe_power_identities() {
        let rules = safe();
        assert_eq!(optimize("x ^ 1", &rules).pretty(), "x");
        assert_eq!(optimize("y ^ 0", &rules).pretty(), "1");
        assert_eq!(optimize("1 ^ z", &rules).pretty(), "1");
    }

    #[test]
    fn a_zero_base_needs_a_provably_positive_exponent() {
        let rules = safe();
        // `min`/`max` are the cheapest way to hand the interval analysis a
        // bound it can use: a bare variable could be anything, NaN included.
        assert_eq!(optimize("0 ^ max(min(k, 3), 1)", &rules).pretty(), "0");
        assert!(optimize("0 ^ k", &rules).pretty().contains('^'));
        // A non-positive exponent is the case the guard is there for: `0 ^ 0`
        // is 1 and `0 ^ -1` is an infinity, neither of them zero.
        assert!(optimize("0 ^ min(max(k, -3), -1)", &rules)
            .pretty()
            .contains('^'));
    }

    #[test]
    fn literal_arguments_simplify_without_constant_folding() {
        let rules = safe();
        assert_eq!(optimize_with("exp(0)", &rules, false).pretty(), "1");
        assert_eq!(optimize_with("ln(1)", &rules, false).pretty(), "0");
        assert_eq!(optimize_with("sin(0)", &rules, false).pretty(), "0");
        assert_eq!(optimize_with("cos(0)", &rules, false).pretty(), "1");
        assert_eq!(optimize_with("tan(0)", &rules, false).pretty(), "0");
        assert_eq!(optimize_with("cos(pi)", &rules, false).to_sexp(), "-1");
    }

    #[test]
    fn atan2_is_already_inside_its_own_bounds() {
        let rules = safe();
        let clamped = "min(atan2(max(min(y, 2), 1), max(min(x, 2), 1)), pi)";
        assert!(optimize(clamped, &rules).to_sexp().starts_with("(atan2"));
        // Without the bounds, `min` would be load-bearing: it is what turns a
        // NaN into pi.
        assert!(optimize("min(atan2(y, x), pi)", &rules)
            .to_sexp()
            .starts_with("(min"));
    }

    #[test]
    fn fast_math_cheapens_powers() {
        let rules = fast_math();
        assert_eq!(optimize("x ^ 2", &rules).pretty(), "x * x");
        assert_eq!(optimize("x ^ -1", &rules).pretty(), "1 / x");
        assert_eq!(optimize("x ^ 0.5", &rules).pretty(), "sqrt(x)");
        assert_eq!(optimize("sqrt(z * z)", &rules).pretty(), "abs(z)");
        // Six multiplies would cost less than one `pow`, but nothing here
        // expands an odd exponent, so the best available term is the single
        // folded power.
        assert_eq!(optimize("(x ^ 2) ^ 3", &rules).pretty(), "x ^ 6");
    }

    #[test]
    fn fast_math_collapses_exp_and_ln() {
        let rules = fast_math();
        assert_eq!(optimize("ln(exp(x))", &rules).pretty(), "x");
        assert_eq!(optimize("exp(a) * exp(b)", &rules).pretty(), "exp(a + b)");
        assert_eq!(optimize("exp(a) / exp(b)", &rules).pretty(), "exp(a - b)");
        assert_eq!(optimize("ln(a) - ln(b)", &rules).pretty(), "ln(a / b)");
        assert_eq!(optimize("ln(a) + ln(b)", &rules).pretty(), "ln(a * b)");
    }

    #[test]
    fn exp_of_ln_keeps_its_domain_check() {
        let rules = fast_math();
        assert!(optimize("exp(ln(x))", &rules).pretty().contains("ln"));
        let clamped = optimize("exp(ln(max(min(x, 5), 1)))", &rules).pretty();
        assert!(
            !clamped.contains("ln") && !clamped.contains("exp"),
            "{}",
            clamped
        );
    }

    #[test]
    fn fast_math_trig_identities() {
        let rules = fast_math();
        assert_eq!(optimize("sin(t) ^ 2 + cos(t) ^ 2", &rules).pretty(), "1");
        assert_eq!(
            optimize("sin(t) * sin(t) + cos(t) * cos(t)", &rules).pretty(),
            "1"
        );
        assert_eq!(optimize("sin(u) / cos(u)", &rules).pretty(), "tan(u)");
        assert_eq!(
            optimize("2 * sin(w) * cos(w)", &rules).pretty(),
            "sin(2 * w)"
        );
        // The operands of a commutative node print in e-class id order, which
        // is the order the input introduced them: `2` came first above, `w`
        // comes first here.
        assert_eq!(
            optimize("cos(w) ^ 2 - sin(w) ^ 2", &rules).pretty(),
            "cos(w * 2)"
        );
    }

    /// Saturating with everything in this module must leave `sin(pi)` at the
    /// value it actually has. Claiming it is zero would give the analysis two
    /// literals for one e-class, and it rejects that outright.
    #[test]
    fn nothing_claims_that_sin_pi_is_zero() {
        let mut rules = safe();
        rules.extend(fast_math());
        let folded = optimize("sin(pi)", &rules);
        assert_eq!(
            eval(&folded, &Env::new()).unwrap(),
            std::f64::consts::PI.sin()
        );
        assert!(optimize_with("sin(pi)", &rules, false)
            .pretty()
            .starts_with("sin("));
    }

    /// The module doc claims a domain assumption escapes the subterm that
    /// justified it. This is the shape of it: `x ^ 0.5` is only meaningful for
    /// `x >= 0`, and the fast tier duly concludes `x = abs(x)` everywhere.
    #[test]
    fn fast_math_lets_a_domain_assumption_escape() {
        let mut rules = safe();
        rules.extend(fast_math());
        assert_eq!(
            optimize("x ^ 0.5 * x ^ 0.5 + abs(x)", &rules).pretty(),
            "x + x"
        );
    }

    /// The fast tier is allowed to move the last bits, but a rule that was
    /// merely mistyped would move rather more than that.
    #[test]
    fn fast_math_stays_numerically_close() {
        let rules = fast_math();
        let cases = [
            "ln(exp(x))",
            "exp(x) * exp(y)",
            "sin(x) ^ 2 + cos(x) ^ 2",
            "sin(x) / cos(x)",
            "2 * sin(x) * cos(x)",
            "sqrt(x * x)",
            "x ^ 2",
        ];
        let mut rng = Rng::seed(99);
        for src in cases {
            let before = parse(src).unwrap();
            let after = optimize(src, &rules);
            for _ in 0..200 {
                let mut env = Env::new();
                env.insert(Sym::new("x"), rng.range(-20.0, 20.0));
                env.insert(Sym::new("y"), rng.range(-20.0, 20.0));
                let a = eval(&before, &env).unwrap();
                let b = eval(&after, &env).unwrap();
                assert!(
                    (a - b).abs() <= 1e-9 * a.abs().max(1.0),
                    "`{}` became `{}`: {:e} vs {:e} at {:?}",
                    src,
                    after.pretty(),
                    b,
                    a,
                    env
                );
            }
        }
    }
}
