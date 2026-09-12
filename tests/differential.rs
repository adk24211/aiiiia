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

fn optimize(expr: &RecExpr, rules: &[Rewrite<MathAnalysis>]) -> RecExpr {
    let runner = Runner::default()
        .with_expr(expr)
        .with_iter_limit(6)
        .with_node_limit(4_000)
        .with_time_limit(Duration::from_secs(2))
        .run(rules);
    Extractor::new(&runner.egraph, OpCost).find_best(runner.root()).1
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
    sweep(2, Grammar::default(), 300, &rules::transcendental::safe(), 0.0);
}

#[test]
fn safe_logic_rules_preserve_results_exactly() {
    sweep(3, Grammar::default().with_logic(), 300, &rules::logic::safe(), 0.0);
}

#[test]
fn the_whole_safe_tier_preserves_results_exactly() {
    sweep(4, Grammar::default().with_logic(), 400, &rules::safe(), 0.0);
}

#[test]
fn fast_math_rules_stay_close() {
    // These are allowed to move the last bits, but not to change the answer.
    // A generous tolerance still catches a rule that is simply wrong.
    sweep(5, Grammar::arithmetic(), 200, &rules::all_rules(), 1e-6);
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
            let args: Vec<f64> = prog
                .params()
                .iter()
                .map(|p| env[p])
                .collect();
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
fn optimization_never_grows_the_chosen_cost() {
    // Extraction may only return something at least as cheap as the input,
    // because the input is itself in the e-graph and therefore a candidate.
    use saturn::extract::dag_cost;
    for expr in ExprStream::new(9, Grammar::default(), 5).take(200) {
        let best = optimize(&expr, &rules::safe());
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

#[test]
fn size_extraction_never_grows_the_dag() {
    for expr in ExprStream::new(10, Grammar::arithmetic(), 5).take(200) {
        let runner = Runner::default()
            .with_expr(&expr)
            .with_iter_limit(5)
            .with_node_limit(3_000)
            .with_time_limit(Duration::from_secs(2))
            .run(&rules::safe());
        let (_, best) = Extractor::new(&runner.egraph, AstSize).find_best(runner.root());
        assert!(
            best.dag_size() <= expr.dag_size(),
            "size extraction grew the DAG: {} -> {}",
            expr.dag_size(),
            best.dag_size()
        );
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
