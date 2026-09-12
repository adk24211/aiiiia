//! Using `saturn` as a library: optimize, extract with different cost models,
//! write custom rules, and plug in a custom analysis.
//!
//! Run with `cargo run --example library`.

use saturn::analysis::Analysis;
use saturn::egraph::EGraph;
use saturn::extract::{dag_cost, AstSize, Extractor, OpCost};
use saturn::lang::{ENode, Id};
use saturn::parser::parse;
use saturn::rewrite::Rewrite;
use saturn::rules::{self, is_finite_nonzero};
use saturn::runner::Runner;
use saturn::{rw, MathAnalysis};

fn main() {
    optimizing();
    cost_models_disagree();
    custom_rules();
    custom_analysis();
}

fn optimizing() {
    println!("== optimizing ==");
    let expr = parse("u / w + v / w").expect("valid syntax");
    let runner = Runner::default().with_expr(&expr).run(&rules::safe());
    let (_tree_cost, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
    println!("  {}  ->  {}", expr.pretty(), best.pretty());
    // Extraction minimizes cost over the expanded *tree*; what actually gets
    // evaluated is the shared DAG, so that is the number worth reporting.
    println!(
        "  cost {} -> {}, {} e-nodes after {} iterations ({})",
        dag_cost(&expr, &OpCost),
        dag_cost(&best, &OpCost),
        runner.egraph.stats().nodes,
        runner.iterations.len(),
        runner.stop_reason.as_ref().expect("the runner ran"),
    );
}

fn cost_models_disagree() {
    println!("== the cost model decides ==");
    // The e-graph holds both forms; which one comes out depends entirely on
    // what you are asking to minimize.
    let expr = parse("(a + b) * (a + b)").expect("valid syntax");
    let runner = Runner::default().with_expr(&expr).run(&rules::all_rules());
    let (size, small) = Extractor::new(&runner.egraph, AstSize).find_best(runner.root());
    let (ops, fast) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
    println!("  smallest: {}  (size {})", small.pretty(), size);
    println!("  fastest:  {}  (ops  {})", fast.pretty(), ops);
}

fn custom_rules() {
    println!("== custom rules ==");
    let rules: Vec<Rewrite<MathAnalysis>> = vec![
        rw!("factor"; "?a * ?b + ?a * ?c" => "?a * (?b + ?c)"),
        rw!("double"; "?a + ?a" => "2 * ?a"),
        rw!("cancel"; "?a / ?a" => "1",
            if "?a is finite and nonzero", is_finite_nonzero("?a")),
    ];
    for src in ["p * q + p * r", "t + t", "k / k", "(1 + 1) / (1 + 1)"] {
        let expr = parse(src).expect("valid syntax");
        let runner = Runner::default().with_expr(&expr).run(&rules);
        let (_, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
        println!("  {:<20} -> {}", src, best.pretty());
    }
    // `k / k` stays put: `k` could be zero, infinite, or NaN, and the analysis
    // cannot prove otherwise, so the rule declines to fire. `(1 + 1) / (1 + 1)`
    // does cancel, because there the interval analysis discharges the side
    // condition. And `t + t` stays as it is: the `double` rule fires, but a
    // multiply costs four adds, so extraction keeps the cheaper original --
    // adding an equality never forces you to use it.
}

/// The fewest variable *occurrences* of any term in an e-class.
///
/// Not the number of distinct variables: this counts occurrences, so
/// `x * a + x * b` is four and the factored `x * (a + b)` is three. The point
/// is that the fact improves as rewriting finds better terms, which is exactly
/// the shape an e-class analysis needs.
struct VarCount;

impl Analysis for VarCount {
    type Data = usize;

    fn make(egraph: &EGraph<Self>, node: &ENode) -> usize {
        if node.as_var().is_some() {
            1
        } else {
            node.children().iter().map(|&c| egraph[c].data).sum()
        }
    }

    /// Both facts describe the same value, so keep the more informative one.
    /// Taking the minimum makes this a meet: it only ever decreases, which is
    /// what the fixpoint needs to terminate.
    fn merge(&mut self, a: usize, b: usize) -> usize {
        a.min(b)
    }

    fn modify(_: &mut EGraph<Self>, _: Id) {}
}

fn custom_analysis() {
    println!("== custom analysis ==");
    let src = "x * a + x * b";
    let mut eg: EGraph<VarCount> = EGraph::new(VarCount);
    let root = eg.add_expr(&parse(src).expect("valid syntax"));
    eg.rebuild();
    println!("  {} uses {} variable occurrences", src, eg[root].data);

    // Add the factored form as an equality. The analysis re-runs and the fact
    // improves without anyone recomputing it by hand -- and it improves for
    // every class above this one too.
    let factored = eg.add_expr(&parse("x * (a + b)").expect("valid syntax"));
    eg.union(root, factored);
    eg.rebuild();
    println!("  after learning it equals x * (a + b), {}", eg[root].data);
}
