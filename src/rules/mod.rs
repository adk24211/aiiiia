//! The rule library.
//!
//! Rules are split into two tiers.
//!
//! **Safe** rules preserve the exact IEEE-754 result of the expression for
//! every input, including infinities, NaN, and signed zero — either because
//! the identity holds bit for bit, or because a side condition checks the
//! cases where it would not. `?x - ?x => 0` is only safe when `?x` is
//! provably finite, and the interval analysis is what proves it.
//!
//! **Fast-math** rules are true over the reals but not over floats:
//! reassociation, distribution, `ln(exp(x)) => x`. They change results in the
//! last few bits and occasionally much more than that, and they are the same
//! trade a C compiler makes under `-ffast-math`. Nothing enables them unless
//! you ask.
//!
//! Every rule is checked at load time: a left-hand side that is a bare
//! variable, or a right-hand side using a variable the left does not bind, is
//! a panic rather than a silent misfire.

use crate::analysis::MathAnalysis;
use crate::egraph::EGraph;
use crate::lang::Id;
use crate::pattern::Subst;
use crate::rewrite::Rewrite;
use crate::sym::Sym;

pub mod arith;
pub mod diff;
pub mod logic;
pub mod transcendental;

/// The rule type every module in here produces.
pub type Rule = Rewrite<MathAnalysis>;

// ---------------------------------------------------------------------------
// Side conditions
// ---------------------------------------------------------------------------
//
// Each returns a closure suitable for the conditional form of `rw!`:
//
// ```text
// rw!("cancel"; "?a / ?a" => "1", if "?a is finite and nonzero", is_finite_nonzero("?a"))
// ```

type Cond = Box<dyn Fn(&EGraph<MathAnalysis>, Id, &Subst) -> bool + Send + Sync>;

fn on_var(
    v: &str,
    f: impl Fn(&crate::interval::Interval) -> bool + Send + Sync + 'static,
) -> Cond {
    let sym = Sym::new(v);
    Box::new(move |egraph, _matched, subst| match subst.get(sym) {
        Some(id) => f(&egraph[id].data.range),
        None => false,
    })
}

/// `?v` is provably not zero and not NaN.
pub fn is_nonzero(v: &str) -> Cond {
    on_var(v, |r| r.is_nonzero())
}
/// `?v` is provably finite: not an infinity, not NaN.
pub fn is_finite(v: &str) -> Cond {
    on_var(v, |r| r.is_finite())
}
/// `?v` is provably finite and non-zero — what division cancellation needs.
pub fn is_finite_nonzero(v: &str) -> Cond {
    on_var(v, |r| r.is_finite_nonzero())
}
/// `?v >= 0` and not NaN.
pub fn is_nonneg(v: &str) -> Cond {
    on_var(v, |r| r.is_nonneg())
}
/// `?v > 0` and not NaN.
pub fn is_positive(v: &str) -> Cond {
    on_var(v, |r| r.is_positive())
}
/// `?v <= 0` and not NaN.
pub fn is_nonpos(v: &str) -> Cond {
    on_var(v, |r| r.is_nonpos())
}
/// `?v` is provably never NaN.
pub fn is_not_nan(v: &str) -> Cond {
    on_var(v, |r| r.is_not_nan())
}
/// `?v` is a known finite literal.
pub fn is_const(v: &str) -> Cond {
    on_var(v, |r| r.as_constant().is_some())
}
/// `?v` is a known literal with an integral value.
pub fn is_int_const(v: &str) -> Cond {
    on_var(v, |r| {
        r.as_constant().map(|x| x == x.trunc()).unwrap_or(false)
    })
}

/// `?v` is a known literal satisfying `pred`.
pub fn const_satisfies(v: &str, pred: impl Fn(f64) -> bool + Send + Sync + 'static) -> Cond {
    on_var(v, move |r| r.as_constant().map(&pred).unwrap_or(false))
}

/// Both conditions hold.
pub fn and(a: Cond, b: Cond) -> Cond {
    Box::new(move |eg, id, s| a(eg, id, s) && b(eg, id, s))
}

/// Every condition holds.
pub fn all(conds: Vec<Cond>) -> Cond {
    Box::new(move |eg, id, s| conds.iter().all(|c| c(eg, id, s)))
}

/// The two variables are bound to *different* e-classes. Useful to stop a rule
/// from firing on the degenerate case another rule already handles.
pub fn distinct(a: &str, b: &str) -> Cond {
    let (a, b) = (Sym::new(a), Sym::new(b));
    Box::new(move |eg, _, s| match (s.get(a), s.get(b)) {
        (Some(x), Some(y)) => eg.find(x) != eg.find(y),
        _ => false,
    })
}

/// Applying the rule would not make the matched class any smaller. Used to
/// stop expansive rules from firing on already-minimal terms.
pub fn always() -> Cond {
    Box::new(|_, _, _| true)
}

// ---------------------------------------------------------------------------
// Rule sets
// ---------------------------------------------------------------------------

/// Every float-safe rule: arithmetic, transcendental, logic, and comparison.
pub fn safe() -> Vec<Rule> {
    let mut v = arith::safe();
    v.extend(transcendental::safe());
    v.extend(logic::safe());
    v
}

/// Rules that are true over the reals but change floating-point results.
pub fn fast_math() -> Vec<Rule> {
    let mut v = arith::fast_math();
    v.extend(transcendental::fast_math());
    v.extend(logic::fast_math());
    v
}

/// Rules that eliminate `d(x, e)` by pushing the derivative down to the leaves.
pub fn differentiation() -> Vec<Rule> {
    diff::safe()
}

/// Safe rules plus differentiation — the default for the `saturn` CLI.
pub fn default_rules() -> Vec<Rule> {
    let mut v = safe();
    v.extend(differentiation());
    v
}

/// Everything, including the fast-math tier.
pub fn all_rules() -> Vec<Rule> {
    let mut v = default_rules();
    v.extend(fast_math());
    v
}

/// Look up a named set, for `saturn --rules <name>`.
pub fn named(name: &str) -> Option<Vec<Rule>> {
    Some(match name {
        "safe" => safe(),
        "fast-math" | "fastmath" => fast_math(),
        "diff" | "differentiation" => differentiation(),
        "default" => default_rules(),
        "all" => all_rules(),
        "arith" | "arithmetic" => arith::safe(),
        "transcendental" | "trans" => transcendental::safe(),
        "logic" => logic::safe(),
        "none" => Vec::new(),
        _ => return None,
    })
}

/// The names [`named`] accepts, with a one-line description of each.
pub fn set_names() -> Vec<(&'static str, &'static str)> {
    vec![
        ("safe", "every rule that preserves IEEE-754 results exactly"),
        ("default", "safe + differentiation (the default)"),
        ("diff", "symbolic differentiation only"),
        ("arith", "safe arithmetic identities"),
        ("transcendental", "safe exp/log/pow/sqrt/trig identities"),
        ("logic", "safe comparison, boolean, if, min/max/abs"),
        ("fast-math", "real-valued identities that change float results"),
        ("all", "default + fast-math"),
        ("none", "no rules; just parse, fold constants, and extract"),
    ]
}
