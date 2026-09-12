//! Assumptions: facts the caller supplies, and the rewrites they unlock.

use saturn::analysis::MathAnalysis;
use saturn::check::Checker;
use saturn::extract::OpCost;
use saturn::gen::{ExprStream, Grammar};
use saturn::interval::Interval;
use saturn::lang::RecExpr;
use saturn::parser::parse;
use saturn::rules;
use saturn::runner::Runner;
use saturn::sym::Sym;
use saturn::Assumptions;
use std::time::Duration;

fn optimize(src: &str, facts: &str, set: &str) -> String {
    let expr = parse(src).unwrap();
    let assumptions = saturn::assume::parse(facts).unwrap_or_else(|e| panic!("{}", e));
    let runner = Runner::new(MathAnalysis::assuming(assumptions))
        .with_iter_limit(10)
        .with_node_limit(10_000)
        .with_time_limit(Duration::from_secs(3));
    let (best, _) = saturn::optimize_with(runner, &expr, &rules::named(set).unwrap(), OpCost);
    best.pretty()
}

#[test]
fn without_a_fact_nothing_cancels() {
    // `w` could be zero, infinite or NaN, so none of these is an identity.
    assert_eq!(optimize("w / w * x", "", "safe"), "w / w * x");
    assert_eq!(optimize("x - x", "", "safe"), "x - x");
    assert_eq!(optimize("abs(x)", "", "safe"), "abs(x)");
    assert_eq!(optimize("max(x, 0)", "", "safe"), "max(x, 0)");
}

#[test]
fn a_fact_unlocks_the_guarded_rule() {
    assert_eq!(
        optimize("w / w * x", "finite(w) && nonzero(w)", "safe"),
        "x"
    );
    assert_eq!(optimize("x - x", "finite(x)", "safe"), "0");
    assert_eq!(optimize("abs(x)", "x > 0", "safe"), "x");
    assert_eq!(optimize("max(x, 0)", "x > 0", "safe"), "x");
    assert_eq!(optimize("min(x, 10)", "x < 5", "safe"), "x");
    assert_eq!(optimize("sign(x)", "x < 0", "safe"), "-1");
}

#[test]
fn an_ordering_settles_a_comparison() {
    assert_eq!(optimize("x < 5", "x > 10", "safe"), "0");
    assert_eq!(optimize("x < 5", "x < 1", "safe"), "1");
    assert_eq!(optimize("x == 5", "x > 10", "safe"), "0");
    assert_eq!(optimize("x != 5", "x > 10", "safe"), "1");
    // `>` and `>=` need no rules of their own: the mirroring rules put the
    // flipped comparison in the same class, where the `<` rules fire.
    assert_eq!(optimize("x > 10", "x < 1", "safe"), "0");
    assert_eq!(optimize("x >= 10", "x < 1", "safe"), "0");
}

#[test]
fn a_decidable_condition_removes_the_branch() {
    assert_eq!(optimize("if(x > 0, a, b)", "x > 1", "safe"), "a");
    assert_eq!(optimize("if(x > 0, a, b)", "x < -1", "safe"), "b");
    // An undecidable one keeps both branches.
    let both = optimize("if(x > 0, a, b)", "finite(x)", "safe");
    assert!(both.contains('a') && both.contains('b'), "{}", both);
}

#[test]
fn a_weaker_fact_does_not_suffice() {
    // `x >= 0` admits `-0.0`, since `-0.0 >= 0.0`. Both rules turn on exactly
    // that: `sign(-0.0)` is `+0.0`, not `-1`, and `abs(-0.0)` is `+0.0`, which
    // is not the `-0.0` it was given.
    assert_eq!(optimize("sign(x)", "x >= 0", "safe"), "sign(x)");
    assert_eq!(optimize("abs(x)", "x >= 0", "safe"), "abs(x)");
    assert_eq!(optimize("abs(x)", "x > 0", "safe"), "x");
    assert_eq!(optimize("abs(x)", "x >= 0 && nonzero(x)", "safe"), "x");
    // Non-zero without a bound says nothing about infinities.
    assert_eq!(optimize("x - x", "nonzero(x)", "safe"), "x - x");
    assert_eq!(optimize("x - x", "finite(x)", "safe"), "0");
}

#[test]
fn facts_combine_across_flags_and_variables() {
    let mut facts: Assumptions = saturn::assume::parse("x >= 0").unwrap();
    for (var, fact) in saturn::assume::parse("nonzero(x)").unwrap() {
        facts
            .entry(var)
            .and_modify(|e| *e = e.meet(fact))
            .or_insert(fact);
    }
    assert!(facts[&Sym::new("x")].is_positive());
}

#[test]
fn an_assumption_does_not_escape_its_variable() {
    // Saying something about `x` must not license anything about `y`.
    assert_eq!(
        optimize("y / y", "finite(x) && nonzero(x)", "safe"),
        "y / y"
    );
    assert_eq!(optimize("x / x", "finite(x) && nonzero(x)", "safe"), "1");
}

#[test]
fn assumptions_survive_the_analysis_being_recomputed() {
    // The analysis recomputes a class from its nodes whenever anything below
    // it changes. An assumption written into a class once would be erased;
    // it has to be part of what a variable node means.
    let expr = parse("(x + 0) / (x + 0)").unwrap();
    let facts = saturn::assume::parse("x > 1, x < 2").unwrap();
    let runner = Runner::new(MathAnalysis::assuming(facts))
        .with_iter_limit(12)
        .with_node_limit(10_000);
    let (best, _) = saturn::optimize_with(runner, &expr, &rules::safe(), OpCost);
    assert_eq!(best.pretty(), "1");
}

#[test]
fn optimizing_under_a_true_assumption_preserves_results() {
    // The safe tier still preserves IEEE-754 results exactly -- but only over
    // inputs that satisfy what was assumed. This is the whole contract:
    // assumptions are taken on trust, and a false one makes the result wrong.
    let facts = saturn::assume::parse("x > 0.5, x < 2, y > 0.5, y < 2, z > 0.5, z < 2").unwrap();
    let checker = Checker::new()
        .with_samples(150)
        .with_seed(0xA55)
        .with_tolerance(0.0)
        .with_range(0.5, 2.0);
    for expr in ExprStream::new(0xA55, Grammar::default(), 5).take(250) {
        let runner = Runner::new(MathAnalysis::assuming(facts.clone()))
            .with_iter_limit(6)
            .with_node_limit(4_000)
            .with_time_limit(Duration::from_secs(2));
        let (best, _): (RecExpr, _) = saturn::optimize_with(runner, &expr, &rules::safe(), OpCost);
        let report = checker.compare(&expr, &best);
        assert!(
            report.ok(),
            "an assumption changed the answer inside its own range\n  {}\n  {}\n{}",
            expr.pretty(),
            best.pretty(),
            report.render()
        );
    }
}

#[test]
fn a_contradictory_assumption_does_not_explode() {
    // `x > 5 && x < 1` describes nothing. The domain has no bottom element to
    // report that with, so it must at least stay sound and terminate.
    let facts = saturn::assume::parse("x > 5, x < 1").unwrap();
    let range: Interval = facts[&Sym::new("x")];
    let expr = parse("x / x + x - x").unwrap();
    let runner = Runner::new(MathAnalysis::assuming(facts))
        .with_iter_limit(8)
        .with_node_limit(5_000)
        .with_time_limit(Duration::from_secs(2));
    let (best, r) = saturn::optimize_with(runner, &expr, &rules::safe(), OpCost);
    assert!(r.stop_reason.is_some());
    assert!(!best.is_empty());
    let _ = range;
}
