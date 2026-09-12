//! Comparisons, boolean connectives, `if`, and the piecewise operators.
//!
//! Booleans are floats: `0.0` is false, every other non-NaN value is true, and
//! NaN is *false*. Comparisons, `!`, `&&` and `||` answer with exactly `0.0`
//! or `1.0`, but nothing forces their operands to be normalized, which is why
//! `!!c` is not the identity and why several rules below carry a side
//! condition that the rest of the language would not need.
//!
//! NaN is where most of these identities go wrong, and almost always in the
//! same way: NaN compares false against everything, so `!(a < b)` and
//! `a >= b` disagree exactly there. Such a rule stays in [`safe`] with an
//! `is_not_nan` guard rather than being demoted to [`fast_math`] — guarded, it
//! holds bit for bit, and the interval analysis discharges the guard often
//! enough to be worth having (every comparison result is provably non-NaN, for
//! one).
//!
//! `min` and `max` need the same care for a different reason: IEEE-754 leaves
//! the result of `min(+0.0, -0.0)` unspecified, so a rule may not assume which
//! zero comes back. Rules that only ever return one of the operands are fine;
//! the ones that would have to *predict* which are guarded with `is_nonzero`,
//! which rules the ambiguity out. Three rules need one more property of a
//! `+0.0`/`-0.0` tie: that `min` and `max` resolve it *consistently* — `max`
//! never handing back the more negative zero of the two, and negating both
//! operands exchanging the two results. IEEE-754 promises neither, and the
//! two differ between a folded and a computed `min` even on one target, so
//! `minmax_ties_agree` below checks them directly.

use super::{
    and, bounds, const_satisfies, is_nonneg, is_nonzero, is_not_nan, is_positive, on_range, on_var,
    Cond, Rule,
};
use crate::analysis::MathAnalysis;
use crate::egraph::EGraph;
use crate::lang::{Id, Op};
use crate::pattern::Subst;
use crate::rw;
use crate::sym::Sym;

/// `?v` is provably strictly negative — the mirror of [`super::is_positive`],
/// which the shared helpers do not spell out. Plain `is_nonpos` is not enough
/// for `abs(?x) => -?x`, because it admits `-0.0`, where `abs` gives `+0.0`
/// and negation gives `-0.0`.
fn is_negative(v: &str) -> Cond {
    on_var(v, |r| r.is_negative())
}

/// `?v` provably denotes exactly `+0.0` or `1.0`.
///
/// The interval analysis answers `[0, 1]` for every comparison and connective,
/// and that is *not* enough to justify `!!c => c`: `[0, 1]` also contains
/// `0.5`, where `!!c` is `1.0`, and `-0.0`, where `!!c` is `+0.0`. What does
/// justify it is the shape of the term — an operator that is boolean by
/// construction — so this looks for one instead of asking the analysis.
fn is_normalized_bool(v: &str) -> Cond {
    let sym = Sym::new(v);
    Box::new(move |egraph: &EGraph<MathAnalysis>, _: Id, subst: &Subst| {
        let Some(id) = subst.get(sym) else {
            return false;
        };
        egraph[id].nodes.iter().any(|n| match n.op {
            Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne => true,
            Op::And | Op::Or | Op::Not => true,
            Op::Const(c) => c.get() == 1.0 || (c.get() == 0.0 && c.get().is_sign_positive()),
            _ => false,
        })
    })
}

/// Every logic rule that preserves the exact IEEE-754 result.
pub fn safe() -> Vec<Rule> {
    vec![
        // -- if ------------------------------------------------------------
        // --- what the analysis can settle outright ------------------------
        //
        // These fire only when the intervals put one value strictly on one
        // side of the other, which for a bare variable means the caller said
        // so. They are the rules an assumption buys.
        //
        // Every one demands that neither side may be NaN: against NaN every
        // comparison is false, `min` and `max` return the other operand, and
        // none of the orderings below mean anything.
        rw!("lt-known-true"; "?a < ?b" => "1",
            if "every ?a is below every ?b",
            bounds("?a", "?b", |a, b| !a.nan && !b.nan && a.hi < b.lo)),
        rw!("lt-known-false"; "?a < ?b" => "0",
            if "no ?a is below any ?b",
            bounds("?a", "?b", |a, b| !a.nan && !b.nan && a.lo >= b.hi)),
        rw!("le-known-true"; "?a <= ?b" => "1",
            if "every ?a is at most every ?b",
            bounds("?a", "?b", |a, b| !a.nan && !b.nan && a.hi <= b.lo)),
        rw!("le-known-false"; "?a <= ?b" => "0",
            if "every ?a is above every ?b",
            bounds("?a", "?b", |a, b| !a.nan && !b.nan && a.lo > b.hi)),
        // Disjoint ranges cannot hold equal values. `>=` and `>` need no twin:
        // `lt-to-gt` and friends put the mirrored comparison in the same
        // class, where these already fire.
        rw!("eq-known-false"; "?a == ?b" => "0",
        if "?a and ?b have disjoint ranges",
        bounds("?a", "?b", |a, b| {
            !a.nan && !b.nan && (a.hi < b.lo || b.hi < a.lo)
        })),
        rw!("ne-known-true"; "?a != ?b" => "1",
        if "?a and ?b have disjoint ranges",
        bounds("?a", "?b", |a, b| {
            !a.nan && !b.nan && (a.hi < b.lo || b.hi < a.lo)
        })),
        // Strict, not `<=`: at a tie the two could be zeros of opposite sign,
        // and `min` is specified to return the negative one rather than the
        // first.
        rw!("min-known"; "min(?a, ?b)" => "?a",
            if "every ?a is strictly below every ?b",
            bounds("?a", "?b", |a, b| !a.nan && !b.nan && a.hi < b.lo)),
        rw!("max-known"; "max(?a, ?b)" => "?a",
            if "every ?a is strictly above every ?b",
            bounds("?a", "?b", |a, b| !a.nan && !b.nan && a.lo > b.hi)),
        // A condition the analysis can decide makes the branch unconditional.
        // `is_nonzero` already excludes NaN, which is the other falsy value.
        rw!("if-known-true"; "if(?c, ?a, ?b)" => "?a",
            if "?c is never zero or NaN", is_nonzero("?c")),
        rw!("if-known-false"; "if(?c, ?a, ?b)" => "?b",
            if "?c is always zero", on_range("?c", |r| r.is_zero())),
        rw!("if-same"; "if(?c, ?a, ?a)" => "?a"),
        rw!("if-const-true"; "if(?c, ?a, ?b)" => "?a",
            if "?c is a constant that is not zero", const_satisfies("?c", |x| x != 0.0)),
        // A constant condition is only ever folded away when *all three*
        // children are constants, so this pair is not subsumed by the
        // analysis. `as_constant` never reports NaN, so `x == 0.0` really does
        // mean falsy here, and it catches `-0.0` as well.
        rw!("if-const-false"; "if(?c, ?a, ?b)" => "?b",
            if "?c is the constant zero", const_satisfies("?c", |x| x == 0.0)),
        rw!("if-of-if-then"; "if(?c, if(?c, ?a, ?b), ?d)" => "if(?c, ?a, ?d)"),
        rw!("if-of-if-else"; "if(?c, ?a, if(?c, ?b, ?d))" => "if(?c, ?a, ?d)"),
        rw!("if-not-cond"; "if(!?c, ?a, ?b)" => "if(?c, ?b, ?a)"),
        // `if` returns a branch untouched, so these two hold for every
        // condition including NaN, which selects the else branch.
        rw!("if-zero-one"; "if(?c, 0, 1)" => "!?c"),
        rw!("if-one-zero"; "if(?c, 1, 0)" => "!(!?c)"),
        // -- boolean connectives -------------------------------------------
        rw!("not-not"; "!(!?c)" => "?c",
            if "?c is already 0 or 1", is_normalized_bool("?c")),
        rw!("de-morgan-and"; "!(?a && ?b)" => "!?a || !?b"),
        rw!("de-morgan-or"; "!(?a || ?b)" => "!?a && !?b"),
        rw!("not-and-not"; "!?a && !?b" => "!(?a || ?b)"),
        rw!("not-or-not"; "!?a || !?b" => "!(?a && ?b)"),
        // `a && a` is not `a` — it is `a` normalized to 0 or 1, which is what
        // `!!a` spells. The same goes for the identity-element rules below.
        rw!("and-same"; "?a && ?a" => "!(!?a)"),
        rw!("or-same"; "?a || ?a" => "!(!?a)"),
        rw!("and-one"; "?a && 1" => "!(!?a)"),
        rw!("or-zero"; "?a || 0" => "!(!?a)"),
        rw!("and-zero"; "?a && 0" => "0"),
        rw!("or-one"; "?a || 1" => "1"),
        // NaN is false and `!NaN` is true, so the excluded middle survives it.
        rw!("and-not-self"; "?a && !?a" => "0"),
        rw!("or-not-self"; "?a || !?a" => "1"),
        // -- comparisons ---------------------------------------------------
        // `==` and `!=` are exact complements even at NaN, which is the one
        // pair of comparisons that needs no guard.
        rw!("not-eq"; "!(?a == ?b)" => "?a != ?b"),
        rw!("not-ne"; "!(?a != ?b)" => "?a == ?b"),
        // The ordered comparisons are all false at NaN, so negating one is not
        // its opposite there: `!(NaN < 1)` is 1 while `NaN >= 1` is 0.
        //
        // `!(?a > ?b)` and `!(?a >= ?b)` get no rules of their own: the
        // reversal rules below put `?b < ?a` in the class of `?a > ?b`, which
        // is where `not-lt` fires, and `ge-to-le` turns its result back round.
        // `negating_a_reversed_comparison` checks that that really happens.
        rw!("not-lt"; "!(?a < ?b)" => "?a >= ?b",
            if "neither side is NaN", and(is_not_nan("?a"), is_not_nan("?b"))),
        rw!("not-le"; "!(?a <= ?b)" => "?a > ?b",
            if "neither side is NaN", and(is_not_nan("?a"), is_not_nan("?b"))),
        // Reversing an ordered comparison is exact at NaN too — both spellings
        // are false — and it is what lets the rules written for `<` see terms
        // that were written with `>`.
        rw!("lt-to-gt"; "?a < ?b" => "?b > ?a"),
        rw!("gt-to-lt"; "?a > ?b" => "?b < ?a"),
        rw!("le-to-ge"; "?a <= ?b" => "?b >= ?a"),
        rw!("ge-to-le"; "?a >= ?b" => "?b <= ?a"),
        // `x < x` is false for every x, NaN included; `x <= x` is not.
        rw!("lt-self"; "?x < ?x" => "0"),
        rw!("le-self"; "?x <= ?x" => "1", if "?x is never NaN", is_not_nan("?x")),
        rw!("eq-self"; "?x == ?x" => "1", if "?x is never NaN", is_not_nan("?x")),
        rw!("ne-self"; "?x != ?x" => "0", if "?x is never NaN", is_not_nan("?x")),
        rw!("lt-or-eq"; "(?a < ?b) || (?a == ?b)" => "?a <= ?b"),
        rw!("le-and-ge"; "(?a <= ?b) && (?a >= ?b)" => "?a == ?b"),
        rw!("lt-or-gt"; "(?a < ?b) || (?a > ?b)" => "?a != ?b",
            if "neither side is NaN", and(is_not_nan("?a"), is_not_nan("?b"))),
        // -- min and max ---------------------------------------------------
        rw!("min-same"; "min(?a, ?a)" => "?a"),
        rw!("max-same"; "max(?a, ?a)" => "?a"),
        // `min(a, min(a, b))` is the *same call* as the inner one once the
        // inner result is known, so whichever zero a tied inner `min` returned
        // comes back out again: no assumption about ties is needed.
        rw!("min-of-min"; "min(?a, min(?a, ?b))" => "min(?a, ?b)"),
        rw!("max-of-max"; "max(?a, max(?a, ?b))" => "max(?a, ?b)"),
        // Absorption does have to predict the tie: with `a = +0.0, b = -0.0`,
        // `max` may hand back `-0.0` and `min` may then keep it, which is not
        // `a`. A non-zero `a` makes every comparison involved strict.
        rw!("min-of-max"; "min(?a, max(?a, ?b))" => "?a",
            if "?a is never zero or NaN", is_nonzero("?a")),
        rw!("max-of-min"; "max(?a, min(?a, ?b))" => "?a",
            if "?a is never zero or NaN", is_nonzero("?a")),
        // `min` and `max` return the two operands in some order, so the sum is
        // the same sum — unless both operands are zeros of opposite sign, when
        // the pair may come back as two `-0.0`s and turn `+0.0` into `-0.0`.
        rw!("min-plus-max"; "min(?a, ?b) + max(?a, ?b)" => "?a + ?b",
            if "?a is never zero or NaN and ?b is never NaN",
            and(is_nonzero("?a"), is_not_nan("?b"))),
        // `max - min` is whichever of `a - b`, `b - a` is non-negative, and
        // `abs` erases the sign. The case that needs more than that is a
        // `+0.0`/`-0.0` tie, where the subtraction must not come out as
        // `-0.0`: `max` never returns the more negative zero of a tie, so the
        // difference is `+0.0`, which is the zero `abs` produces.
        rw!("max-minus-min"; "max(?a, ?b) - min(?a, ?b)" => "abs(?a - ?b)",
            if "neither side is NaN", and(is_not_nan("?a"), is_not_nan("?b"))),
        // Negating both operands exchanges `min` and `max`, a `+0.0`/`-0.0`
        // tie included: `min(-a, -b)` gives back the negation of whichever
        // zero `max(a, b)` chose.
        rw!("min-of-negs"; "min(-?a, -?b)" => "-max(?a, ?b)"),
        rw!("max-of-negs"; "max(-?a, -?b)" => "-min(?a, ?b)"),
        // -- abs and sign --------------------------------------------------
        rw!("abs-abs"; "abs(abs(?x))" => "abs(?x)"),
        rw!("abs-neg"; "abs(-?x)" => "abs(?x)"),
        // `is_nonneg` would not do: it admits `-0.0`, and `abs(-0.0)` is
        // `+0.0`. Strict positivity excludes both zeros.
        rw!("abs-positive"; "abs(?x)" => "?x", if "?x > 0", is_positive("?x")),
        rw!("abs-negative"; "abs(?x)" => "-?x", if "?x < 0", is_negative("?x")),
        // Squaring erases the sign anyway, including for `-0.0`.
        rw!("abs-times-abs"; "abs(?x) * abs(?x)" => "?x * ?x"),
        rw!("sign-positive"; "sign(?x)" => "1", if "?x > 0", is_positive("?x")),
        rw!("sign-negative"; "sign(?x)" => "-1", if "?x < 0", is_negative("?x")),
        rw!("sign-sign"; "sign(sign(?x))" => "sign(?x)"),
        rw!("sign-abs"; "sign(abs(?x))" => "abs(sign(?x))"),
        // -- floor and ceil ------------------------------------------------
        rw!("floor-floor"; "floor(floor(?x))" => "floor(?x)"),
        rw!("ceil-ceil"; "ceil(ceil(?x))" => "ceil(?x)"),
        // Whatever `floor` returns is already integral (or an infinity, or
        // NaN), and every one of those is a fixed point of `ceil`, down to the
        // sign of a zero.
        rw!("ceil-floor"; "ceil(floor(?x))" => "floor(?x)"),
        rw!("floor-ceil"; "floor(ceil(?x))" => "ceil(?x)"),
        rw!("floor-sign"; "floor(sign(?x))" => "sign(?x)"),
        rw!("ceil-sign"; "ceil(sign(?x))" => "sign(?x)"),
        rw!("neg-floor-neg"; "-floor(-?x)" => "ceil(?x)"),
        rw!("neg-ceil-neg"; "-ceil(-?x)" => "floor(?x)"),
    ]
}

/// Logic rules that are true over the reals but change float results.
///
/// Every one of these is wrong only at a zero of the wrong sign or at NaN —
/// the two places where the float encoding of "the smaller of two numbers"
/// stops agreeing with the mathematical one.
pub fn fast_math() -> Vec<Rule> {
    vec![
        // `if(a < b, a, b)` returns `b` when either side is NaN; `min` returns
        // the operand that is not NaN. They also disagree on which zero comes
        // back when `a` and `b` are `+0.0` and `-0.0`.
        rw!("if-lt-min"; "if(?a < ?b, ?a, ?b)" => "min(?a, ?b)"),
        rw!("if-lt-max"; "if(?a < ?b, ?b, ?a)" => "max(?a, ?b)"),
        rw!("if-gt-max"; "if(?a > ?b, ?a, ?b)" => "max(?a, ?b)"),
        rw!("if-gt-min"; "if(?a > ?b, ?b, ?a)" => "min(?a, ?b)"),
        // Sound for every value except `-0.0`, which `abs` normalizes to
        // `+0.0` and this rule would leave negative.
        rw!("abs-nonneg"; "abs(?x)" => "?x", if "?x >= 0", is_nonneg("?x")),
        rw!("minmax-sum"; "min(?a, ?b) + max(?a, ?b)" => "?a + ?b"),
        // `sign(-0.0)` is `+0.0`, so the product loses the sign of a zero; at
        // NaN both sides are NaN and agree.
        rw!("sign-times-abs"; "sign(?x) * abs(?x)" => "?x"),
        // `0 / 0` and `inf / inf` are NaN where `sign` answers `0` and `1`.
        rw!("div-by-abs"; "?x / abs(?x)" => "sign(?x)"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{eval, Env};
    use crate::extract::{Extractor, OpCost};
    use crate::parser::parse;
    use crate::runner::{Runner, StopReason};

    /// The inputs every rule is brute-forced over: both zeros, an integral and
    /// a non-integral magnitude of each sign, both infinities, and NaN.
    const VALUES: [f64; 9] = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        2.5,
        -2.5,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];

    /// Saturate `src` and print the cheapest equivalent term.
    fn optimized(src: &str, rules: &[Rule]) -> String {
        let expr = parse(src).unwrap();
        let runner = Runner::default().with_expr(&expr).run(rules);
        let (_, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
        best.pretty()
    }

    /// Did saturating `src` prove it equal to `other`?
    fn proves_equal(src: &str, other: &str, rules: &[Rule]) -> bool {
        let expr = parse(src).unwrap();
        let mut runner = Runner::default().with_expr(&expr).run(rules);
        let want = parse(other).unwrap();
        let id = runner.egraph.add_expr(&want);
        runner.egraph.rebuild();
        runner.egraph.equivalent(id, runner.root())
    }

    /// Bit equality, except that all NaNs count as one value: neither the
    /// payload nor the sign of a NaN is observable through this language.
    fn same_value(a: f64, b: f64) -> bool {
        if a.is_nan() || b.is_nan() {
            a.is_nan() && b.is_nan()
        } else {
            a.to_bits() == b.to_bits()
        }
    }

    /// The two sides of a rule as text, and whether it is guarded.
    ///
    /// A conditional applier describes itself as `<rhs> if <why>`, and no
    /// right-hand side in this file contains ` if ` — an `if(...)` never has a
    /// space before it — so the first separator is the real one. A mis-split
    /// would leave text that does not parse, which the callers turn into a
    /// failure rather than a silent skip.
    fn sides(rule: &Rule) -> (String, String, bool) {
        let lhs = rule.searcher.to_string_pretty();
        let described = rule.applier.describe();
        match described.split_once(" if ") {
            Some((rhs, _)) => (lhs, rhs.to_string(), true),
            None => (lhs, described, false),
        }
    }

    /// The value bound to pattern variable `v`.
    fn val(env: &Env, v: &str) -> f64 {
        env[&Sym::new(v)]
    }

    /// What [`super::is_not_nan`] admits.
    fn not_nan(env: &Env, v: &str) -> bool {
        !val(env, v).is_nan()
    }

    /// What [`super::is_nonzero`] admits: neither zero, and not NaN.
    fn nonzero(env: &Env, v: &str) -> bool {
        let x = val(env, v);
        x != 0.0 && !x.is_nan()
    }

    /// Each guarded rule's side condition, rewritten as a predicate on the
    /// values its variables take: the reasoning the side conditions encode,
    /// written out. Every one must admit *everything* the interval predicate
    /// admits, since tightening one here would quietly stop testing cases the
    /// rule still claims. `as_constant` only ever reports a finite double,
    /// which is why the two `if-const` guards may ask for `is_finite`.
    fn guard_for(name: &str) -> Option<fn(&Env) -> bool> {
        let f: fn(&Env) -> bool = match name {
            "if-const-true" => |e: &Env| val(e, "?c").is_finite() && val(e, "?c") != 0.0,
            "if-const-false" => |e: &Env| val(e, "?c").is_finite() && val(e, "?c") == 0.0,
            "not-not" => |e: &Env| {
                let c = val(e, "?c");
                c == 1.0 || (c == 0.0 && c.is_sign_positive())
            },
            "not-lt" | "not-le" | "lt-or-gt" | "max-minus-min" => {
                |e: &Env| not_nan(e, "?a") && not_nan(e, "?b")
            }
            "le-self" | "eq-self" | "ne-self" => |e: &Env| not_nan(e, "?x"),
            "min-of-max" | "max-of-min" => |e: &Env| nonzero(e, "?a"),
            "min-plus-max" => |e: &Env| nonzero(e, "?a") && not_nan(e, "?b"),
            "abs-positive" | "sign-positive" => |e: &Env| val(e, "?x") > 0.0,
            "abs-negative" | "sign-negative" => |e: &Env| val(e, "?x") < 0.0,
            // The rules that fire when the analysis can order the two values.
            // Here the ordering is the concrete one, which is what the
            // intervals are an approximation of.
            "lt-known-true" | "min-known" => |e: &Env| ordered(e) && val(e, "?a") < val(e, "?b"),
            "lt-known-false" => |e: &Env| ordered(e) && val(e, "?a") >= val(e, "?b"),
            "le-known-true" => |e: &Env| ordered(e) && val(e, "?a") <= val(e, "?b"),
            "le-known-false" | "max-known" => |e: &Env| ordered(e) && val(e, "?a") > val(e, "?b"),
            "eq-known-false" | "ne-known-true" => {
                |e: &Env| ordered(e) && val(e, "?a") != val(e, "?b")
            }
            "if-known-true" => |e: &Env| not_nan(e, "?c") && val(e, "?c") != 0.0,
            "if-known-false" => |e: &Env| not_nan(e, "?c") && val(e, "?c") == 0.0,
            _ => return None,
        };
        Some(f)
    }

    /// Neither of the two compared values is NaN, which is what every
    /// ordering below silently assumes: against NaN each comparison is false
    /// and `min` and `max` return the other operand.
    fn ordered(e: &Env) -> bool {
        not_nan(e, "?a") && not_nan(e, "?b")
    }

    /// Assert that `lhs` and `rhs` evaluate to the same double for every
    /// assignment of [`VALUES`] to their variables that `guard` admits, and
    /// return how many assignments that was.
    fn assert_exact(name: &str, lhs_text: &str, rhs_text: &str, guard: fn(&Env) -> bool) -> usize {
        let lhs =
            parse(lhs_text).unwrap_or_else(|e| panic!("rule `{}` lhs `{}`: {}", name, lhs_text, e));
        let rhs =
            parse(rhs_text).unwrap_or_else(|e| panic!("rule `{}` rhs `{}`: {}", name, rhs_text, e));
        let vars = lhs.vars();
        let mut admitted = 0;
        for i in 0..VALUES.len().pow(vars.len() as u32) {
            let mut env = Env::new();
            let mut k = i;
            for v in &vars {
                env.insert(*v, VALUES[k % VALUES.len()]);
                k /= VALUES.len();
            }
            if !guard(&env) {
                continue;
            }
            admitted += 1;
            let l = eval(&lhs, &env).unwrap();
            let r = eval(&rhs, &env).unwrap();
            assert!(
                same_value(l, r),
                "rule `{}` ({} => {}) gives {} vs {} at {:?}",
                name,
                lhs_text,
                rhs_text,
                l,
                r,
                vars.iter()
                    .map(|v| (v.as_str(), env[v]))
                    .collect::<Vec<_>>()
            );
        }
        admitted
    }

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
        assert_eq!(names.len(), total, "duplicate rule name in logic rules");
    }

    /// The rule set must reach a fixpoint rather than grow forever: the
    /// comparison-flipping and De Morgan rules each add a term that the other
    /// direction then finds again, which is exactly how a rule set fails to
    /// terminate if the two are not inverses.
    #[test]
    fn the_rules_saturate() {
        let mut rules = safe();
        rules.extend(fast_math());
        let expr = parse(
            "if(!(a < b) && !(c || d), min(max(x, y), max(x, y)), abs(-floor(-z)) == sign(w))",
        )
        .unwrap();
        let runner = Runner::default().with_expr(&expr).run(&rules);
        assert_eq!(
            runner.stop_reason,
            Some(StopReason::Saturated),
            "{}",
            runner.report()
        );
    }

    #[test]
    fn extracts_the_expected_term() {
        let rules = safe();
        for (src, want) in [
            ("if(c, y, y)", "y"),
            ("if(1 < 2, p, q)", "p"),
            ("if(c, 0, 1)", "!c"),
            ("if(c, if(c, a, b), d)", "if(c, a, d)"),
            ("x && 0", "0"),
            ("x || 1", "1"),
            ("x < x", "0"),
            ("!(a == b)", "a != b"),
            ("!p && !q", "!(p || q)"),
            ("min(a, min(a, b))", "min(a, b)"),
            ("max(-p, -q)", "-min(p, q)"),
            ("abs(abs(w))", "abs(w)"),
            ("abs(-w)", "abs(w)"),
            ("floor(sign(t))", "sign(t)"),
            ("-floor(-w)", "ceil(w)"),
        ] {
            assert_eq!(optimized(src, &rules), want, "optimizing `{}`", src);
        }
    }

    #[test]
    fn fast_math_turns_a_select_into_min() {
        let mut rules = safe();
        rules.extend(fast_math());
        assert_eq!(optimized("if(a < b, a, b)", &rules), "min(a, b)");
        assert_eq!(optimized("if(a > b, a, b)", &rules), "max(a, b)");
        // The safe tier must not make that leap on its own.
        assert!(!proves_equal("if(a < b, a, b)", "min(a, b)", &safe()));
    }

    #[test]
    fn nan_guards_hold_the_unsound_rewrites_back() {
        let rules = safe();
        // Nothing is known about `x` and `y`, so both may be NaN and
        // `!(x < y)` must stay put.
        assert!(!proves_equal("!(x < y)", "x >= y", &rules));
        assert!(!proves_equal("x == x", "1", &rules));
        // A comparison result is never NaN, so the same rules fire here.
        assert!(proves_equal(
            "!((a < b) < (c < d))",
            "(a < b) >= (c < d)",
            &rules
        ));
        assert!(proves_equal("(a < b) == (a < b)", "1", &rules));
    }

    /// `!(?a > ?b)` and `!(?a >= ?b)` have no rule of their own; reversing the
    /// comparison first is what reaches them. If that ever stops working the
    /// two rules have to come back.
    #[test]
    fn negating_a_reversed_comparison() {
        let rules = safe();
        assert!(proves_equal(
            "!((a < b) > (c < d))",
            "(a < b) <= (c < d)",
            &rules
        ));
        assert!(proves_equal(
            "!((a < b) >= (c < d))",
            "(a < b) < (c < d)",
            &rules
        ));
        // And the NaN guard still holds them back on unknown operands.
        assert!(!proves_equal("!(x > y)", "x <= y", &rules));
    }

    #[test]
    fn conditions_that_the_analysis_can_discharge() {
        let rules = safe();
        // A comparison lands in `[0, 1]` and is never NaN, so `(a < b) + 1`
        // is provably in `[1, 2]` and `abs` and `sign` both collapse.
        assert!(proves_equal("abs((a < b) + 1)", "(a < b) + 1", &rules));
        assert!(proves_equal("sign((a < b) + 1)", "1", &rules));
        // Nothing is known about `x`, so `abs(x)` may be a negative zero as
        // far as `abs-positive` is concerned, and it must not fire.
        assert!(!proves_equal("abs(abs(x))", "x", &rules));
        // `min(-2, ...)` is negative whatever the other operand is.
        assert!(proves_equal("abs(min(-2, y))", "-min(-2, y)", &rules));
    }

    #[test]
    fn boolean_normalization() {
        let rules = safe();
        // `!!c` is `c` only once `c` is known to be 0 or 1.
        assert!(!proves_equal("!(!x)", "x", &rules));
        assert!(proves_equal("!(!(x < y))", "x < y", &rules));
        assert!(proves_equal("!(!(!x))", "!x", &rules));
        assert!(proves_equal("x && x", "!(!x)", &rules));
        assert!(proves_equal("(x < y) && 1", "x < y", &rules));
    }

    /// Every rule without a side condition must be exact on every input,
    /// which is small enough a claim to check by brute force.
    #[test]
    fn unconditional_safe_rules_are_exact() {
        let mut checked = 0;
        for rule in safe() {
            let (lhs, rhs, guarded) = sides(&rule);
            if guarded {
                continue;
            }
            assert_exact(&rule.name, &lhs, &rhs, |_: &Env| true);
            checked += 1;
        }
        // Guard against the split above quietly skipping everything.
        assert!(checked >= 20, "only {} rules were checked", checked);
    }

    /// The same brute force for the guarded rules, over the inputs their side
    /// conditions admit. Every guarded rule needs an entry in [`guard_for`],
    /// so a new one cannot be added without writing down what makes it exact.
    #[test]
    fn guarded_safe_rules_are_exact_where_guarded() {
        let mut checked = 0;
        for rule in safe() {
            let (lhs, rhs, guarded) = sides(&rule);
            if !guarded {
                continue;
            }
            let guard = guard_for(&rule.name).unwrap_or_else(|| {
                panic!(
                    "rule `{}` is guarded but this test has no predicate for its side condition",
                    rule.name
                )
            });
            let admitted = assert_exact(&rule.name, &lhs, &rhs, guard);
            assert!(
                admitted > 0,
                "rule `{}`: its guard admits nothing",
                rule.name
            );
            checked += 1;
        }
        assert!(checked >= 15, "only {} guarded rules were checked", checked);
    }

    /// `max-minus-min`, `min-of-negs` and `max-of-negs` are the three rules
    /// whose exactness needs `min` and `max` to resolve a `+0.0`/`-0.0` tie
    /// consistently with each other. IEEE-754 allows an implementation where
    /// they do not, so this states what those rules rely on — which zero each
    /// call returns is deliberately left open.
    #[test]
    fn minmax_ties_agree() {
        for (a, b) in [(0.0f64, -0.0f64), (-0.0f64, 0.0f64)] {
            assert!(
                same_value(a.max(b) - a.min(b), 0.0),
                "max-minus-min: a tie must subtract to +0.0, not -0.0"
            );
            assert!(
                same_value((-a).min(-b), -a.max(b)),
                "min-of-negs: negating the operands must exchange min and max"
            );
            assert!(
                same_value((-a).max(-b), -a.min(b)),
                "max-of-negs: negating the operands must exchange max and min"
            );
        }
    }
}
