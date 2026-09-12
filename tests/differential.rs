//! The claim that the safe rule tier preserves IEEE-754 results, tested.
//!
//! For thousands of randomly generated expressions, saturate with the safe
//! rules, extract, and compare the result against the original over random
//! inputs. A safe-tier disagreement is a soundness bug in a rule; these tests
//! are the only thing standing between the rule library and silently wrong
//! answers.

use saturn::analysis::MathAnalysis;
use saturn::check::{agree, Checker};
use saturn::eval::{eval, Env};
use saturn::extract::{AstSize, Extractor, OpCost};
use saturn::gen::{ExprStream, Grammar};
use saturn::lang::RecExpr;
use saturn::rewrite::Rewrite;
use saturn::rng::Rng;
use saturn::rules;
use saturn::runner::Runner;
use saturn::sym::Sym;
use saturn::vm::Program;
use std::time::Duration;

/// Limits that keep a sweep of hundreds of expressions quick without changing
/// what any of these tests prove.
fn bounded() -> Runner<MathAnalysis> {
    Runner::default()
        .with_iter_limit(6)
        .with_node_limit(4_000)
        .with_time_limit(Duration::from_secs(2))
}

fn optimize(expr: &RecExpr, rules: &[Rewrite<MathAnalysis>]) -> RecExpr {
    saturn::optimize_with(bounded(), expr, rules, OpCost).0
}

/// Saturate `count` generated expressions and assert the result still agrees
/// with the original everywhere.
fn sweep(seed: u64, grammar: Grammar, count: usize, rules: &[Rewrite<MathAnalysis>], tol: f64) {
    let checker = Checker::new()
        .with_samples(120)
        .with_seed(seed ^ 0x9E37_79B9)
        .with_tolerance(tol)
        .with_wild(true);
    for (i, expr) in ExprStream::new(seed, grammar, 5).take(count).enumerate() {
        let best = optimize(&expr, rules);
        let report = checker.compare(&expr, &best);
        assert!(
            report.ok(),
            "expression {} changed meaning\n  input:     {}\n  optimized: {}\n{}",
            i,
            expr.pretty(),
            best.pretty(),
            report.render()
        );
    }
}

#[test]
fn safe_arithmetic_rules_preserve_results_exactly() {
    // Tolerance zero: the safe tier claims bit-for-bit equality, so anything
    // looser would not be testing the claim.
    sweep(1, Grammar::arithmetic(), 300, &rules::arith::safe(), 0.0);
}

#[test]
fn safe_transcendental_rules_preserve_results_exactly() {
    sweep(
        2,
        Grammar::default(),
        300,
        &rules::transcendental::safe(),
        0.0,
    );
}

#[test]
fn safe_logic_rules_preserve_results_exactly() {
    sweep(
        3,
        Grammar::default().with_logic(),
        300,
        &rules::logic::safe(),
        0.0,
    );
}

#[test]
fn the_whole_safe_tier_preserves_results_exactly() {
    sweep(4, Grammar::default().with_logic(), 400, &rules::safe(), 0.0);
}

#[test]
fn fast_math_rules_are_algebraically_correct() {
    // Two allowances, both of them the point of the tier rather than
    // concessions. Fast-math is licensed to turn a NaN or an infinity into a
    // number -- `?x / ?x => 1` is exactly that -- so only inputs where both
    // sides produce an ordinary number say anything about the rules. And it
    // reassociates, which is algebraically exact and numerically wrong
    // whenever a sum cancels, so inputs are drawn from one narrow band of
    // magnitudes: that measures whether the algebra is right rather than how
    // badly cancellation bites.
    let checker = Checker::new()
        .with_samples(120)
        .with_seed(5 ^ 0x9E37_79B9)
        .with_tolerance(1e-6)
        .with_range(0.5, 2.0)
        .with_finite_only(true);
    let rules = rules::all_rules();
    for (i, expr) in ExprStream::new(5, Grammar::arithmetic(), 5)
        .take(150)
        .enumerate()
    {
        let best = optimize(&expr, &rules);
        let report = checker.compare(&expr, &best);
        assert!(
            report.ok(),
            "expression {} changed a finite answer\n  input:     {}\n  optimized: {}\n{}",
            i,
            expr.pretty(),
            best.pretty(),
            report.render()
        );
    }
}

#[test]
fn constant_folding_alone_is_exact() {
    // With no rules at all, the only transformation is the e-class analysis
    // folding constants. That must never change a value.
    sweep(6, Grammar::default().with_logic(), 300, &[], 0.0);
}

#[test]
fn the_compiler_reproduces_the_interpreter_bit_for_bit() {
    let mut rng = Rng::seed(0xC0DE);
    let grammar = Grammar::default();
    for expr in ExprStream::new(8, grammar, 5).take(400) {
        let prog = Program::compile(&expr).expect("no derivative nodes are generated here");
        let vars = expr.vars();
        for _ in 0..20 {
            let values: Vec<f64> = vars.iter().map(|_| rng.float()).collect();
            let env: Env = vars.iter().copied().zip(values.iter().copied()).collect();
            let interpreted = eval(&expr, &env).expect("every variable is bound");
            let args: Vec<f64> = prog.params().iter().map(|p| env[p]).collect();
            let compiled = prog.eval(&args);
            assert!(
                interpreted.to_bits() == compiled.to_bits()
                    || (interpreted.is_nan() && compiled.is_nan()),
                "compiled result differs\n  expr: {}\n  args: {:?}\n  interpreted: {:?}\n  compiled:    {:?}",
                expr.pretty(),
                env.iter().map(|(k, v)| (k.to_string(), *v)).collect::<Vec<_>>(),
                interpreted,
                compiled
            );
        }
    }
}

#[test]
fn optimizing_never_returns_something_more_expensive() {
    // The input is itself in the e-graph, so it is always a candidate, and
    // `optimize` keeps it when the extractor's answer is dearer.
    use saturn::extract::dag_cost;
    for rules in [rules::safe(), rules::all_rules()] {
        for expr in ExprStream::new(9, Grammar::default(), 5).take(120) {
            let best = optimize(&expr, &rules);
            let before = dag_cost(&expr, &OpCost);
            let after = dag_cost(&best, &OpCost);
            assert!(
                after <= before + 1e-9,
                "optimizing made it worse\n  {} ({})\n  {} ({})",
                expr.pretty(),
                before,
                best.pretty(),
                after
            );
        }
    }
}

#[test]
fn tree_extraction_is_optimal_for_tree_cost() {
    // What the bottom-up fixpoint actually minimizes is cost over the expanded
    // tree, and there it is exact: the input is in the e-graph, so the result
    // can never be a more expensive tree.
    use saturn::extract::tree_cost;
    for expr in ExprStream::new(10, Grammar::arithmetic(), 5).take(200) {
        let runner = bounded().with_expr(&expr).run(&rules::safe());
        let (cost, best) = Extractor::new(&runner.egraph, AstSize).find_best(runner.root());
        assert!(
            cost <= tree_cost(&expr, &AstSize) + 1e-9,
            "tree extraction grew the tree: {} -> {}",
            tree_cost(&expr, &AstSize),
            cost
        );
        assert!(best.dag_size() >= 1);
    }
}

#[test]
fn dag_extraction_is_never_worse_than_tree_extraction() {
    use saturn::extract::{dag_cost, DagExtractor};
    for expr in ExprStream::new(12, Grammar::default(), 5).take(100) {
        let runner = bounded().with_expr(&expr).run(&rules::all_rules());
        let (_, tree) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
        let (dag_reported, dag) =
            DagExtractor::new(&runner.egraph, OpCost).find_best(runner.root());
        assert!(
            dag_cost(&dag, &OpCost) <= dag_cost(&tree, &OpCost) + 1e-9,
            "refinement made the DAG worse\n  tree pick: {}\n  dag pick:  {}",
            tree.pretty(),
            dag.pretty()
        );
        assert!((dag_reported - dag_cost(&dag, &OpCost)).abs() < 1e-9);
    }
}

#[test]
fn differentiation_matches_a_numeric_derivative() {
    // A central difference is only good to about six digits, so this is a
    // sanity check on the rules rather than a precision test -- but it would
    // catch a sign error or a missing chain-rule factor immediately.
    let cases = [
        "x * x",
        "x ^ 3",
        "1 / x",
        "sqrt(x)",
        "exp(x)",
        "ln(x)",
        "sin(x)",
        "cos(x)",
        "sin(x) * cos(x)",
        "exp(sin(x))",
        "x * x * x + 2 * x",
        "(x + 1) / (x - 1)",
        "sqrt(x * x + 1)",
        "exp(x) / (exp(x) + 1)",
    ];
    let x = Sym::new("x");
    for case in cases {
        let wrapped = saturn::parser::parse(&format!("d(x, {})", case)).unwrap();
        let derivative = optimize(&wrapped, &rules::default_rules());
        let original = saturn::parser::parse(case).unwrap();

        for at in [0.37f64, 1.4, 2.9, 5.5] {
            let h = 1e-5 * at.abs().max(1.0);
            let env_at = |v: f64| -> Env { [(x, v)].into_iter().collect() };
            let Ok(plus) = eval(&original, &env_at(at + h)) else {
                continue;
            };
            let Ok(minus) = eval(&original, &env_at(at - h)) else {
                continue;
            };
            if !plus.is_finite() || !minus.is_finite() {
                continue;
            }
            let numeric = (plus - minus) / (2.0 * h);
            let symbolic = eval(&derivative, &env_at(at))
                .unwrap_or_else(|e| panic!("d/dx {} at {}: {}", case, at, e));
            assert!(
                agree(numeric, symbolic, 1e-4),
                "d/dx {} at {}\n  symbolic: {} -> {}\n  numeric:  {}",
                case,
                at,
                derivative.pretty(),
                symbolic,
                numeric
            );
        }
    }
}

#[test]
fn no_derivative_node_survives_saturation() {
    use saturn::lang::Op;
    let grammar = Grammar::default().with_derivatives();
    let mut left = 0;
    for expr in ExprStream::new(11, grammar, 4).take(150) {
        if !expr
            .reachable(expr.root())
            .iter()
            .any(|&id| expr.node(id).op == Op::Diff)
        {
            continue;
        }
        let best = optimize(&expr, &rules::default_rules());
        if best
            .reachable(best.root())
            .iter()
            .any(|&id| best.node(id).op == Op::Diff)
        {
            left += 1;
        }
    }
    assert_eq!(
        left, 0,
        "{} generated expressions still contained a derivative after saturation",
        left
    );
}
