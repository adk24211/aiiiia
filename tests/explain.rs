//! Derivations: why the engine says two expressions are equal.

use saturn::analysis::MathAnalysis;
use saturn::egraph::EGraph;
use saturn::explain::Justification;
use saturn::extract::{AstSize, DagExtractor, OpCost};
use saturn::lang::{ENode, RecExpr};
use saturn::parser::parse;
use saturn::rules;
use saturn::runner::Runner;
use std::time::Duration;

fn run(a: &str, b: &str, set: &str) -> (Runner<MathAnalysis>, RecExpr, RecExpr) {
    let (ea, eb) = (parse(a).unwrap(), parse(b).unwrap());
    let runner = Runner::default()
        .with_explanations()
        .with_expr(&ea)
        .with_expr(&eb)
        .with_iter_limit(8)
        .with_node_limit(5_000)
        .with_time_limit(Duration::from_secs(3))
        .run(&rules::named(set).unwrap());
    (runner, ea, eb)
}

fn names(runner: &Runner<MathAnalysis>) -> Vec<String> {
    runner
        .explain_roots(0, 1)
        .expect("the two should be provably equal")
        .rule_counts()
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn a_single_rule_derivation_names_that_rule() {
    let (runner, _, _) = run("x * y + x * z", "x * (y + z)", "all");
    let e = runner.explain_roots(0, 1).expect("equal");
    assert_eq!(e.len(), 1, "{:?}", e);
    assert!(matches!(
        e.steps[0].justification,
        Justification::Rule { .. }
    ));
    assert_eq!(e.rule_counts(), vec![("distribute".to_string(), 1)]);
}

#[test]
fn the_bindings_are_recorded() {
    let (runner, _, _) = run("exp(a) * exp(b)", "exp(a + b)", "all");
    let e = runner.explain_roots(0, 1).expect("equal");
    let Justification::Rule { rule, subst } = &e.steps[0].justification else {
        panic!("expected a rule step, got {:?}", e.steps[0].justification);
    };
    assert_eq!(rule.name, "exp-prod");
    assert_eq!(subst.len(), 2, "both pattern variables should be bound");

    let extractor = DagExtractor::new(&runner.egraph, AstSize);
    let bound: Vec<String> = {
        let mut v: Vec<String> = subst
            .iter()
            .map(|(_, id)| extractor.find_best(id).1.pretty())
            .collect();
        v.sort();
        v
    };
    assert_eq!(bound, vec!["a", "b"]);
}

#[test]
fn a_congruence_unfolds_into_its_arguments() {
    // The two roots differ only deep inside, so the top-level step is a
    // congruence and the rule that did the work is nested under it.
    let (runner, _, _) = run("(a * b) * c + 1", "a * (b * c) + 1", "all");
    let e = runner.explain_roots(0, 1).expect("equal");
    assert!(e.total_steps() > e.len(), "nothing was nested: {:?}", e);
    let rules: Vec<String> = e.rule_counts().into_iter().map(|(n, _)| n).collect();
    assert!(rules.iter().any(|n| n == "assoc-mul"), "{:?}", rules);
    assert!(rules.iter().any(|n| n == "congruence"), "{:?}", rules);
}

#[test]
fn commutativity_alone_needs_no_derivation() {
    // `a * b` and `b * a` are the same e-node, not two nodes a rule connects,
    // so there is nothing to prove and the derivation is empty.
    let (runner, _, _) = run("(a * b) * c", "c * (b * a)", "all");
    let e = runner.explain_roots(0, 1).expect("equal");
    assert!(e.is_empty(), "{:?}", e);
}

#[test]
fn a_swap_inside_a_congruence_does_not_hide_the_derivation() {
    // Congruent nodes routinely differ by a swap, because commutative children
    // are stored in a canonical order. Pairing arguments by position would
    // then compare terms that are not equal, find nothing, and report a
    // congruence that needed no explanation -- hiding the whole proof.
    let (runner, _, _) = run(
        "a*x^3 + b*x^2 + c*x + d",
        "x * (c + x * (b + x * a)) + d",
        "all",
    );
    let e = runner.explain_roots(0, 1).expect("equal");
    let rules: Vec<String> = e.rule_counts().into_iter().map(|(n, _)| n).collect();
    assert!(
        rules.iter().any(|n| n != "congruence"),
        "the derivation collapsed to bare congruence: {:?}",
        rules
    );
    assert!(e.total_steps() > 1, "{:?}", e);
}

#[test]
fn constant_folding_is_a_step_like_any_other() {
    // The analysis runs at rebuild, so the two are still separate classes
    // when the caller takes hold of them, and the fold that joins them is a
    // step the derivation can name.
    let mut egraph: EGraph<MathAnalysis> = EGraph::default();
    egraph.enable_explanations();
    let product = egraph.add_expr(&parse("2 * 3").unwrap());
    let six = egraph.add_constant(6.0);
    assert_ne!(product, six, "folding should not have happened yet");
    egraph.rebuild();
    assert!(egraph.equivalent(product, six));
    let e = egraph.explain(product, six).expect("folding proves it");
    assert!(
        e.steps
            .iter()
            .any(|s| matches!(s.justification, Justification::Fold)),
        "{:?}",
        e
    );
}

#[test]
fn an_asserted_equality_says_so() {
    let mut egraph: EGraph<MathAnalysis> = EGraph::default();
    egraph.enable_explanations();
    let x = egraph.add(ENode::var("x"));
    let y = egraph.add(ENode::var("y"));
    egraph.union(x, y);
    egraph.rebuild();
    let e = egraph.explain(x, y).expect("they were unioned");
    assert!(matches!(e.steps[0].justification, Justification::Asserted));
}

#[test]
fn unequal_expressions_have_no_derivation() {
    let (runner, _, _) = run("x + 1", "x + 2", "all");
    assert!(runner.explain_roots(0, 1).is_none());
}

#[test]
fn identical_expressions_need_no_derivation() {
    let (runner, _, _) = run("sqrt(a) + b", "b + sqrt(a)", "none");
    let e = runner.explain_roots(0, 1).expect("the same term");
    assert!(e.is_empty(), "{:?}", e);
}

#[test]
fn nothing_is_recorded_unless_it_was_asked_for() {
    let expr = parse("x * y + x * z").unwrap();
    let runner = Runner::default()
        .with_expr(&expr)
        .with_iter_limit(4)
        .run(&rules::safe());
    assert!(!runner.egraph.explanations_enabled());
    let root = runner.root();
    assert!(runner.egraph.explain(root, root).is_none());
}

#[test]
fn explaining_does_not_change_what_is_found() {
    // Recording reasons must be observation only. If it changed the search,
    // the derivation would be of a different run than the one it describes.
    for src in [
        "u / w + v / w",
        "a*x^3 + b*x^2 + c*x + d",
        "exp(a) * exp(b) * exp(c)",
        "sin(t)*sin(t) + cos(t)*cos(t)",
    ] {
        let expr = parse(src).unwrap();
        let plain = Runner::default()
            .with_expr(&expr)
            .with_iter_limit(6)
            .with_node_limit(5_000)
            .run(&rules::all_rules());
        let explained = Runner::default()
            .with_explanations()
            .with_expr(&expr)
            .with_iter_limit(6)
            .with_node_limit(5_000)
            .run(&rules::all_rules());
        assert_eq!(
            plain.egraph.stats(),
            explained.egraph.stats(),
            "recording reasons changed the graph for `{}`",
            src
        );
        let a = DagExtractor::new(&plain.egraph, OpCost)
            .find_best(plain.root())
            .1;
        let b = DagExtractor::new(&explained.egraph, OpCost)
            .find_best(explained.root())
            .1;
        assert_eq!(a.to_sexp(), b.to_sexp(), "different result for `{}`", src);
    }
}

#[test]
fn a_derivation_terminates_on_deeply_nested_congruence() {
    // Each congruence unfolds into its arguments, which are usually congruent
    // in turn. Without a bound this would follow the expression to its leaves.
    let (runner, _, _) = run(
        "sqrt(sqrt(sqrt(sqrt((a * b) * c))))",
        "sqrt(sqrt(sqrt(sqrt(a * (b * c)))))",
        "all",
    );
    let e = runner.explain_roots(0, 1).expect("equal");
    assert!(e.total_steps() < 100, "{} steps", e.total_steps());
    let text = e.render(&|id| {
        DagExtractor::new(&runner.egraph, AstSize)
            .find_best(id)
            .1
            .pretty()
    });
    assert!(!text.is_empty());
}

#[test]
fn every_rule_named_in_a_derivation_really_exists() {
    let all = rules::all_rules();
    let known: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
    for (a, b) in [
        ("x * y + x * z", "x * (y + z)"),
        ("ln(a) - ln(b)", "ln(a / b)"),
        ("(x ^ 2) ^ 3", "x ^ 6"),
        ("u / w + v / w", "(u + v) / w"),
    ] {
        let (runner, _, _) = run(a, b, "all");
        for name in names(&runner) {
            assert!(
                name == "congruence"
                    || name == "constant folding"
                    || known.contains(&name.as_str()),
                "`{}` is not a rule in the set",
                name
            );
        }
    }
}
