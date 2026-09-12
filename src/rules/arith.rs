//! Arithmetic identities: `+`, `-`, `*`, `/`, unary negation, and how each of
//! them interacts with the literals `0`, `1`, `-1` and `2`.
//!
//! # What can be safe at all
//!
//! Four families of float operations are *exact*, and every rule in [`safe`]
//! is built out of them:
//!
//! * **Sign flips.** Negation, and multiplication or division by `-1`, change
//!   the sign bit and nothing else. They never round, never overflow, and map
//!   every input — subnormal, infinite, NaN — onto a value with the same
//!   magnitude. This is why the whole `a - b` / `a + -b` / `-1 * a` cluster
//!   is interchangeable term for term.
//! * **Multiplying or dividing by a power of two**, of which `* 1`, `/ 1` and
//!   the doubling rule `a + a => 2 * a` are the cases worth writing down.
//!   Scaling by a power of two is exact until it overflows, and it overflows
//!   identically on both sides of these rules.
//! * **Adding a zero to a non-zero value**, which returns that value untouched.
//! * **Cancelling a value against itself**, where the exact answer needs no
//!   rounding at all — but only once the value is known to be finite, since
//!   `inf - inf` and `0 / 0` are NaN rather than `0` and `1`.
//!
//! Everything else rounds, and rounding is what makes float `+` and `*`
//! non-associative and non-distributive: `(a + b) + c` rounds the partial sum
//! `a + b` before it ever sees `c`, while `a + (b + c)` rounds a different
//! partial sum, and the two answers can differ by far more than an ulp when
//! the sum cancels. `a * (b + c)` rounds once where `a * b + a * c` rounds
//! three times. So reassociation, distribution, factoring, and every rule that
//! cancels a multiply against a divide live in [`fast_math`].
//!
//! # The one bit this language cannot carry
//!
//! [`F`](crate::sym::F) compares and hashes floats by their bit pattern, with
//! one collapse: every NaN maps to a single canonical pattern, because `Eq`
//! demands reflexivity and IEEE-754 forbids it. So the e-graph cannot hold two
//! distinct NaNs and no side condition can tell them apart.
//!
//! The arithmetic underneath is not so tidy. `Op::eval` hands back whatever
//! NaN the hardware produced, and `-x` flips a NaN's sign bit where `x * -1`
//! leaves it alone. *Exact* in this file therefore means exact up to the sign
//! bit of a NaN result — a bit the sign-flip rules do change, and one that
//! neither `F`, the printer, nor the differential checker reads.
//!
//! A zero's sign, by contrast, is carried faithfully. `-0.0` and `0.0` are
//! distinct literals, distinct e-nodes, and distinct e-classes, because they
//! are distinguishable: `1 / -0.0` is `-inf`. A `0` in a pattern matches only
//! `+0.0`. What still needs care is a signed zero arriving through a
//! *variable*, which is why `?x + 0 => ?x` is guarded — two zeros of opposite
//! sign add to `+0.0` — while `?x - 0 => ?x`, exact at both zeros, is not.
//!
use super::{and, is_const, is_finite, is_finite_nonzero, is_nonzero, is_positive, Rule};
use crate::analysis::MathAnalysis;
use crate::egraph::EGraph;
use crate::lang::{Id, Op};
use crate::pattern::{Pattern, Subst};
use crate::rewrite::{DynamicApplier, Rewrite};
use crate::sym::Sym;
use crate::{rw, rw_bi};

/// Largest exponent [`expand_pow`] will turn into a multiplication chain.
///
/// Every factor in the chain rounds again, so the expansion drifts further
/// from `pow`'s single rounding the larger the exponent gets. The cap also
/// keeps the rule from flooding the graph: each expansion adds multiplies that
/// the reassociation rules then have to chew through.
const MAX_POW_EXPANSION: u32 = 8;

/// Identities that reproduce the original expression's IEEE-754 result on
/// every input, for every rule.
///
/// Bit for bit, with the one exception the module docs set out: the sign bit
/// of a NaN, which the sign-flip rules may change and nothing downstream
/// reads.
pub fn safe() -> Vec<Rule> {
    let mut rules = vec![
        // `x + 0.0` returns `x` untouched for every x except `-0.0`, where
        // round-to-nearest gives `+0.0`: two zeros of opposite sign cancel
        // their signs rather than keeping one. Proving `?x` non-zero rules
        // that out.
        rw!("add-zero"; "?x + 0" => "?x", if "?x is nonzero", is_nonzero("?x")),
        // Subtraction needs no such guard: `x - 0.0` is `x + -0.0`, and
        // `-0.0 + -0.0` is `-0.0`, so the identity holds at both zeros. The
        // literal here is `+0.0` and only `+0.0`.
        rw!("sub-zero"; "?x - 0" => "?x"),
        // `x * 0.0` is `+0.0` for every finite positive x, and something else
        // for each of the cases the guard excludes: `-0.0` for a negative x,
        // NaN for an infinity or a NaN.
        rw!("mul-zero"; "?x * 0" => "0",
            if "?x is finite and positive", and(is_finite("?x"), is_positive("?x"))),
        // `0.0 / x` is `+0.0` whenever x is positive, infinities included.
        // A negative x gives `-0.0`, and a zero gives NaN.
        rw!("zero-div"; "0 / ?x" => "0", if "?x is positive", is_positive("?x")),
        // `0 - x` and `-x` disagree at `x = 0`: subtraction yields `+0.0`,
        // negation yields `-0.0`. Away from zero the subtraction is exact.
        rw!("zero-sub"; "0 - ?x" => "-?x", if "?x is nonzero", is_nonzero("?x")),
        // Multiplying or dividing by one preserves the significand, the
        // exponent and the sign bit, so these hold at +/-0, +/-inf and NaN.
        rw!("mul-one"; "?x * 1" => "?x"),
        rw!("div-one"; "?x / 1" => "?x"),
        // Dividing by -1 is exact for the same reason multiplying by -1 is:
        // the quotient's magnitude is the numerator's.
        rw!("div-neg-one"; "?x / -1" => "-?x"),
        rw!("neg-neg"; "-(-?x)" => "?x"),
        // Cancellation is only zero when there is nothing to cancel to:
        // `inf - inf` and `NaN - NaN` are NaN. For finite x the difference is
        // exactly `+0.0`, even at `x = -0.0`.
        rw!("sub-self"; "?x - ?x" => "0", if "?x is finite", is_finite("?x")),
        rw!("div-self"; "?x / ?x" => "1", if "?x is finite and nonzero", is_finite_nonzero("?x")),
        // `pow(x, 1)` is specified to return x for every x, NaN included.
        rw!("pow-one"; "?x ^ 1" => "?x"),
    ];

    // Doubling is exact in both directions: `x + x` has the exact value `2x`,
    // and so does `2 * x`, so both round the same way and overflow to the same
    // infinity. It survives `-0.0` (`-0.0 + -0.0` is `-0.0`) and subnormals,
    // where doubling stays inside the subnormal range.
    rules.extend(rw_bi!("double"; "?x + ?x" => "2 * ?x"));
    // Negation is a sign flip and so is multiplication by -1; on NaN they can
    // differ in the sign bit alone, which this language cannot observe.
    rules.extend(rw_bi!("neg-mul-one"; "?x * -1" => "-?x"));
    // IEEE-754 defines `a - b` to be `a + (-b)`, and `-b` is exact, so these
    // agree on the nose — `inf - inf` gives NaN both ways, and every
    // combination of signed zeros lands on the same zero. Run forwards and
    // then through `neg-neg` it also settles `?a - -?b => ?a + ?b`, so there
    // is no separate rule for that shape.
    rules.extend(rw_bi!("sub-to-add"; "?a - ?b" => "?a + -?b"));
    // The sign of a product or quotient is the exclusive-or of its operands'
    // signs and its magnitude does not depend on them, so a negation moves
    // across a `*` or `/` without changing a single bit of the result.
    rules.extend(rw_bi!("neg-mul"; "-(?a * ?b)" => "-?a * ?b"));
    rules.extend(rw_bi!("neg-div"; "-(?a / ?b)" => "-?a / ?b"));
    rules.push(rw!("neg-div-denom"; "?a / -?b" => "-(?a / ?b)"));
    rules
}

/// Identities that hold over the real numbers but not over floats.
///
/// These are the trade `-ffast-math` makes: reassociation and distribution
/// change the rounding, and the cancellation rules additionally assume no
/// operand is infinite, NaN, or a negatively-signed zero.
pub fn fast_math() -> Vec<Rule> {
    let mut rules = vec![
        // The unguarded forms of the safe rules above. Each is wrong only at
        // an infinity, a NaN, or a signed zero — the values fast-math assumes
        // away.
        rw!("add-zero-lax"; "?x + 0" => "?x"),
        rw!("sub-zero-lax"; "?x - 0" => "?x"),
        rw!("mul-zero-lax"; "?x * 0" => "0"),
        rw!("zero-div-lax"; "0 / ?x" => "0"),
        rw!("sub-self-lax"; "?x - ?x" => "0"),
        rw!("div-self-lax"; "?x / ?x" => "1"),
        // Reassociation. One direction of each is enough: union is symmetric,
        // and the matcher's commutative search reaches the other groupings.
        rw!("assoc-add"; "(?a + ?b) + ?c" => "?a + (?b + ?c)"),
        rw!("assoc-mul"; "(?a * ?b) * ?c" => "?a * (?b * ?c)"),
        // Negation does not distribute over addition when the sum cancels:
        // `-(1 + -1)` is `-0.0` but `-1 + 1` is `+0.0`.
        rw!("neg-sub"; "-(?a - ?b)" => "?b - ?a"),
        rw!("neg-add-fold"; "-?a + -?b" => "-(?a + ?b)"),
        rw!("distribute"; "?a * (?b + ?c)" => "?a * ?b + ?a * ?c"),
        rw!("factor"; "?a * ?b + ?a * ?c" => "?a * (?b + ?c)"),
        rw!("distribute-sub"; "?a * (?b - ?c)" => "?a * ?b - ?a * ?c"),
        rw!("factor-sub"; "?a * ?b - ?a * ?c" => "?a * (?b - ?c)"),
        // Pulling a constant coefficient out of a sum. Requiring `?k` to be a
        // literal keeps this from matching every addition in the graph, and
        // lets the analysis fold `?k + 1` immediately.
        rw!("factor-const"; "?k * ?a + ?a" => "(?k + 1) * ?a", if "?k is a literal", is_const("?k")),
        // Fusing two constant scalings. Both factors must be literals, or this
        // reassociates a product of three unknowns and loses the rounding that
        // the written order asked for.
        rw!(
            "fuse-const-mul";
            "(?k1 * ?x) * ?k2" => "(?k1 * ?k2) * ?x",
            if "?k1 and ?k2 are literals",
            and(is_const("?k1"), is_const("?k2"))
        ),
        rw!("split-div"; "(?a + ?b) / ?c" => "?a / ?c + ?b / ?c"),
        rw!("join-div"; "?a / ?c + ?b / ?c" => "(?a + ?b) / ?c"),
        // Nested division. Both forms round twice, but at different points,
        // and `?b * ?c` can overflow where the two separate divides would not.
        rw!("div-div-left"; "?a / ?b / ?c" => "?a / (?b * ?c)"),
        rw!("div-div-right"; "?a / (?b / ?c)" => "?a * ?c / ?b"),
        // Cancelling a multiply against a divide is the classic unsound one:
        // `(a * b) / b` overflows to infinity for large `a * b`, is NaN at
        // `b = 0`, and rounds twice where `a` rounds not at all.
        rw!("mul-div-cancel"; "?a * ?b / ?b" => "?a"),
        // `?c` matches `?b` as happily as anything else, so this reaches
        // `?a / ?b * ?b` and hands it to `mul-div-cancel`; the divide-first
        // spelling needs no rule of its own.
        rw!("mul-over-div"; "?a / ?b * ?c" => "?a * ?c / ?b"),
        // Introducing a power. `pow` is not required to be correctly rounded,
        // so even `pow(x, 2)` may not be the correctly rounded square that
        // `x * x` is.
        rw!("square"; "?x * ?x" => "?x ^ 2"),
        rw!("mul-pow"; "?x * ?x ^ ?k" => "?x ^ (?k + 1)", if "?k is a literal", is_const("?k")),
    ];
    rules.push(expand_pow());
    rules
}

/// `?x ^ k` for a small integral `k`, as a chain of multiplications.
///
/// A pattern cannot express this: the number of factors is only known once
/// `?k` is bound. The chain is built by repeated squaring, so `x ^ 8` costs
/// three multiplies rather than seven — and rounds three times rather than
/// seven, which is why this is the cheapest correct-over-the-reals expansion
/// and still not exact.
fn expand_pow() -> Rule {
    let base = Sym::new("?x");
    let exponent = Sym::new("?k");
    Rewrite::new(
        "expand-pow",
        Pattern::parse("?x ^ ?k").unwrap_or_else(|e| panic!("rule `expand-pow` lhs: {}", e)),
        Box::new(DynamicApplier {
            f: Box::new(
                move |egraph: &mut EGraph<MathAnalysis>, matched: Id, subst: &Subst| {
                    let (Some(x), Some(k)) = (subst.get(base), subst.get(exponent)) else {
                        return Vec::new();
                    };
                    let Some(k) = egraph[k].data.value() else {
                        return Vec::new();
                    };
                    if k != k.trunc() || k < 2.0 || k > f64::from(MAX_POW_EXPANSION) {
                        return Vec::new();
                    }
                    let chain = power_chain(egraph, x, k as u32);
                    if egraph.union(matched, chain) {
                        vec![chain]
                    } else {
                        Vec::new()
                    }
                },
            ),
            description: format!(
                "a chain of multiplications, for an integral exponent in 2..={}",
                MAX_POW_EXPANSION
            ),
        }),
    )
    .unwrap_or_else(|e| panic!("{}", e))
}

/// `base` multiplied by itself `k` times, by repeated squaring.
fn power_chain(egraph: &mut EGraph<MathAnalysis>, base: Id, k: u32) -> Id {
    debug_assert!(k >= 1, "a power chain needs at least one factor");
    if k == 1 {
        return base;
    }
    if k % 2 == 0 {
        let half = power_chain(egraph, base, k / 2);
        egraph.add_op(Op::Mul, vec![half, half])
    } else {
        let rest = power_chain(egraph, base, k - 1);
        egraph.add_op(Op::Mul, vec![rest, base])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{eval, Env};
    use crate::extract::{Extractor, OpCost};
    use crate::lang::RecExpr;
    use crate::parser::parse;
    use crate::runner::{BackoffScheduler, Runner};
    use std::collections::HashSet;

    /// Saturating with the fast-math set needs the backoff scheduler.
    ///
    /// Cancellation makes e-classes self-referential — once `?x * 0 => 0`
    /// fires, the zero class contains a multiply whose child is that same
    /// class — and `assoc-mul` then finds exponentially many ways to regroup
    /// inside it. Backing the expansive rules off a few iterations at a time
    /// lets the contracting rules shape the graph first.
    fn runner(src: &str, rules: &[Rule]) -> Runner<MathAnalysis> {
        let expr = parse(src).expect("the test expression should parse");
        Runner::default()
            .with_scheduler(BackoffScheduler::default())
            .with_expr(&expr)
            .run(rules)
    }

    fn saturate(src: &str, rules: &[Rule]) -> RecExpr {
        let runner = runner(src, rules);
        let (_, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
        best
    }

    fn simplify(src: &str, rules: &[Rule]) -> String {
        saturate(src, rules).pretty()
    }

    /// Whether saturating `src` proves it equal to `other`.
    ///
    /// Sharper than comparing extracted strings, which tie-break arbitrarily
    /// between two forms of equal cost.
    fn proves_equal(src: &str, other: &str, rules: &[Rule]) -> bool {
        let mut runner = runner(src, rules);
        let root = runner.root();
        let target = runner
            .egraph
            .add_expr(&parse(other).expect("the test expression should parse"));
        runner.egraph.find(root) == runner.egraph.find(target)
    }

    /// A range the interval analysis can pin down, so the guarded rules fire.
    ///
    /// `min`/`max` return the non-NaN operand, so this is in `[1, 3]` for
    /// every double, NaN included — the guards are never merely *assumed*
    /// here, they are provable.
    const BOUNDED: &str = "max(min(x, 3), 1)";

    /// Every double a rule has to survive: both zeros, both infinities, a
    /// NaN, the subnormal boundary, and a magnitude that overflows on
    /// doubling.
    const SPECIALS: [f64; 13] = [
        -0.0,
        0.0,
        1.0,
        -1.0,
        2.0,
        -3.5,
        f64::MIN_POSITIVE,
        5e-324,
        1e308,
        -1e308,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];

    /// Bit equality, counting every NaN as one value.
    ///
    /// That is the coarsest comparison the safe tier is allowed: a NaN's sign
    /// bit is the only thing these rules may change (see the module docs), so
    /// it is also the only thing this may forgive.
    fn same_value(a: f64, b: f64) -> bool {
        if a.is_nan() || b.is_nan() {
            a.is_nan() && b.is_nan()
        } else {
            a.to_bits() == b.to_bits()
        }
    }

    #[test]
    fn every_rule_has_a_unique_name() {
        let mut seen = HashSet::new();
        for rule in safe().into_iter().chain(fast_math()) {
            assert!(
                seen.insert(rule.name.clone()),
                "duplicate rule `{}`",
                rule.name
            );
        }
        // A `-lax` rule is the unguarded twin of a safe one. Keeping the stems
        // in step is what makes the two tiers comparable at a glance, and a
        // stem that no longer resolves means one side was renamed alone.
        let guarded: HashSet<String> = safe().into_iter().map(|r| r.name).collect();
        for rule in fast_math() {
            if let Some(stem) = rule.name.strip_suffix("-lax") {
                assert!(
                    guarded.contains(stem),
                    "`{}` has no guarded counterpart `{}`",
                    rule.name,
                    stem
                );
            }
            assert!(
                !guarded.contains(&rule.name),
                "`{}` is in both tiers; loading `all` would hide one from the scheduler",
                rule.name
            );
        }
    }

    /// The whole promise of [`safe`], on every rule at once.
    ///
    /// Saturating and re-extracting must not move a bit, so this evaluates
    /// the original and the rewritten form side by side over [`SPECIALS`].
    /// One expression per rule, and the guarded rules get a [`BOUNDED`]
    /// argument so their side conditions actually discharge.
    #[test]
    fn safe_rules_are_exact_on_every_special_value() {
        let safe = safe();
        let mut sources: Vec<String> = [
            "x * 1", "x / 1", "x / -1", "-(-x)", "x ^ 1", "2 * x", "x + x", "x * -1", "-x",
            "x - y", "x + -y", "x - -y", "-(x * y)", "-x * y", "-(x / y)", "-x / y", "x / -y",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        sources.extend([
            format!("{BOUNDED} + 0"),
            format!("{BOUNDED} - 0"),
            format!("0 - {BOUNDED}"),
            format!("{BOUNDED} - {BOUNDED}"),
            format!("{BOUNDED} / {BOUNDED}"),
        ]);

        for src in &sources {
            let before = parse(src).expect("the test expression should parse");
            let after = saturate(src, &safe);
            for &x in SPECIALS.iter() {
                for &y in SPECIALS.iter() {
                    let env: Env = [(Sym::new("x"), x), (Sym::new("y"), y)]
                        .into_iter()
                        .collect();
                    let want = eval(&before, &env).expect("eval");
                    let got = eval(&after, &env).expect("eval");
                    assert!(
                        same_value(want, got),
                        "`{}` became `{}`, which at x = {:e}, y = {:e} gives {:?} not {:?}",
                        src,
                        after.pretty(),
                        x,
                        y,
                        got,
                        want
                    );
                }
            }
        }
    }

    /// The guards in [`safe`] are load-bearing: dropping them really is wrong.
    ///
    /// Each case is an input where the fast-math twin of a guarded rule
    /// changes the answer. A guard that made no difference at any input would
    /// be a guard worth deleting.
    ///
    #[test]
    fn the_guards_are_not_decoration() {
        for (src, x) in [("x + 0", -0.0), ("x - x", f64::INFINITY), ("x / x", 0.0)] {
            let env: Env = [(Sym::new("x"), x)].into_iter().collect();
            let before = eval(&parse(src).expect("parse"), &env).expect("eval");
            let after = eval(&saturate(src, &fast_math()), &env).expect("eval");
            assert!(
                !same_value(before, after),
                "fast-math left `{}` alone at x = {:e}; the safe tier need not guard it",
                src,
                x
            );
        }
    }

    #[test]
    fn unit_and_sign_identities() {
        let safe = safe();
        assert_eq!(simplify("x * 1 / 1", &safe), "x");
        assert_eq!(simplify("-(-x)", &safe), "x");
        assert_eq!(simplify("x * -1", &safe), "-x");
        assert_eq!(simplify("x / -1", &safe), "-x");
        // No rule spells this one out; `sub-to-add` and `neg-neg` compose.
        assert_eq!(simplify("x - -y", &safe), "x + y");
        // A negation costs the same wherever it sits, so which side of the
        // product it ends up on is a tie the extractor breaks arbitrarily.
        assert!(proves_equal("-(x * y)", "-x * y", &safe));
        assert!(proves_equal("x / -y", "-(x / y)", &safe));
        assert!(proves_equal("-x", "-1 * x", &safe));
    }

    #[test]
    fn doubling_runs_both_ways() {
        // A multiply costs four adds, so the extractor prefers the sum.
        assert_eq!(simplify("2 * x", &safe()), "x + x");
    }

    #[test]
    fn cancellation_waits_for_a_proof_of_finiteness() {
        let safe = safe();
        // Nothing bounds `x`, so it may be an infinity or a NaN.
        assert_eq!(simplify("x - x", &safe), "x - x");
        assert_eq!(simplify("x / x", &safe), "x / x");
        // Clamped between 1 and 3, it cannot be either.
        assert_eq!(simplify(&format!("{BOUNDED} - {BOUNDED}"), &safe), "0");
        assert_eq!(simplify(&format!("{BOUNDED} / {BOUNDED}"), &safe), "1");
    }

    #[test]
    fn adding_zero_waits_for_a_proof_of_non_zero() {
        let safe = safe();
        // `-0.0 + 0.0` is `+0.0`, so addition is not the identity at
        // `x = -0.0` and the guard must hold it back. Subtraction is exact at
        // both zeros and needs no guard.
        assert!(!proves_equal("x + 0", "x", &safe));
        assert!(proves_equal("x - 0", "x", &safe));
        assert_eq!(simplify(&format!("{BOUNDED} + 0"), &safe), BOUNDED);
        assert_eq!(
            simplify(&format!("0 - {BOUNDED}"), &safe),
            format!("-{BOUNDED}")
        );

        // Multiplying by zero and dividing zero are exact where the sign of
        // the result is pinned, and held back everywhere else.
        assert!(!proves_equal("x * 0", "0", &safe));
        assert!(!proves_equal("0 / x", "0", &safe));
        assert_eq!(simplify(&format!("{BOUNDED} * 0"), &safe), "0");
        assert_eq!(simplify(&format!("0 / {BOUNDED}"), &safe), "0");
    }

    /// The two zeros are distinguishable, so the core keeps them apart.
    #[test]
    fn the_two_zeros_stay_apart() {
        // `1 / 0.0` is `+inf` and `1 / -0.0` is `-inf`, so conflating the
        // literals would let the e-graph substitute one infinity for the
        // other. No rule in this file may merge them either.
        let mut egraph: EGraph<MathAnalysis> = EGraph::new(MathAnalysis::default());
        let positive = egraph.add_constant(0.0);
        let negative = egraph.add_constant(-0.0);
        egraph.rebuild();
        assert_ne!(egraph.find(positive), egraph.find(negative));

        assert_eq!(simplify("-0 + 0", &safe()), "0");
        assert_eq!(simplify("-0 - 0", &safe()), "-0");
        assert_eq!(simplify("1 / -0", &safe()), "1 / -0");
    }

    #[test]
    fn fast_math_drops_the_guards() {
        let fast = fast_math();
        assert_eq!(simplify("x + 0", &fast), "x");
        assert_eq!(simplify("x - x", &fast), "0");
        assert_eq!(simplify("x / x", &fast), "1");
        assert_eq!(simplify("x * 0", &fast), "0");
    }

    #[test]
    fn fast_math_factors_and_cancels() {
        let fast = fast_math();
        assert_eq!(simplify("x * y + x * z", &fast), "x * (y + z)");
        assert_eq!(simplify("x * y / y", &fast), "x");
        // The divide-first spelling has no rule; `mul-over-div` reorders it
        // into one `mul-div-cancel` can finish.
        assert_eq!(simplify("a / b * b", &fast), "a");
        assert_eq!(simplify("a / b / c", &fast), "a / (b * c)");
    }

    #[test]
    fn constant_scalings_fuse() {
        // `2 * 3` folds on its own once the two literals are adjacent.
        let out = simplify("(2 * x) * 3", &fast_math());
        assert!(
            out == "x * 6" || out == "6 * x",
            "expected a single scaling by 6, got `{}`",
            out
        );
    }

    #[test]
    fn small_powers_become_multiplications() {
        let fast = fast_math();
        for (src, arg, want) in [
            ("x ^ 2", 3.0, 9.0),
            ("x ^ 3", 3.0, 27.0),
            ("x ^ 8", 2.0, 256.0),
        ] {
            let out = saturate(src, &fast);
            assert!(
                out.nodes().iter().all(|n| n.op != Op::Pow),
                "`{}` still contains a power: {}",
                src,
                out.pretty()
            );
            let env: Env = [(Sym::new("x"), arg)].into_iter().collect();
            assert_eq!(eval(&out, &env).expect("eval"), want);
        }
        // `pow-one` is exact, so it does not need the fast-math tier.
        assert_eq!(simplify("x ^ 1", &safe()), "x");
    }

    #[test]
    fn large_powers_are_left_alone() {
        let out = saturate("x ^ 12", &fast_math());
        assert!(
            out.nodes().iter().any(|n| n.op == Op::Pow),
            "an exponent past the expansion limit should survive: {}",
            out.pretty()
        );
    }

    #[test]
    fn repeated_squaring_builds_the_shortest_chain() {
        for (k, multiplies) in [(2u32, 1usize), (3, 2), (4, 2), (5, 3), (8, 3)] {
            let mut fresh: EGraph<MathAnalysis> = EGraph::new(MathAnalysis::default());
            let base = fresh.add_op(Op::Var(Sym::new("x")), Vec::new());
            let root = power_chain(&mut fresh, base, k);
            fresh.rebuild();
            let (_, expr) = Extractor::new(&fresh, OpCost).find_best(root);
            let muls = expr
                .reachable(expr.root())
                .into_iter()
                .filter(|&id| expr.node(id).op == Op::Mul)
                .count();
            assert_eq!(muls, multiplies, "x ^ {} used {} multiplies", k, muls);
        }

        // Both factors are the *same* e-class, not two copies of the base. An
        // `x * x` built from two separate ids would cost the extractor twice
        // and would never match `square`'s repeated `?x`.
        let mut egraph: EGraph<MathAnalysis> = EGraph::new(MathAnalysis::default());
        let x = egraph.add_op(Op::Var(Sym::new("x")), Vec::new());
        let squared = power_chain(&mut egraph, x, 2);
        egraph.rebuild();
        assert_eq!(egraph[squared].nodes[0].children(), &[x, x]);
        assert_eq!(egraph[squared].nodes[0].op, Op::Mul);
    }
}
