//! Invariant and property tests for the e-graph itself.

use saturn::analysis::{MathAnalysis, NoAnalysis};
use saturn::egraph::EGraph;
use saturn::extract::{AstSize, Extractor, OpCost};
use saturn::lang::{ENode, Id, Op};
use saturn::parser::parse;
use saturn::rng::Rng;

fn empty() -> EGraph<NoAnalysis> {
    EGraph::new(NoAnalysis)
}

#[test]
fn adding_the_same_term_twice_creates_nothing() {
    let mut eg = empty();
    let e = parse("sqrt(a * b) + sqrt(a * b)").unwrap();
    let x = eg.add_expr(&e);
    let before = eg.stats();
    let y = eg.add_expr(&e);
    eg.rebuild();
    assert_eq!(x, y);
    assert_eq!(eg.stats(), before, "re-adding grew the graph");
    eg.check_invariants();
}

#[test]
fn congruence_propagates_upward() {
    // Proving a = b must make f(a) = f(b) with no rule involved at all.
    let mut eg = empty();
    let a = eg.add(ENode::var("a"));
    let b = eg.add(ENode::var("b"));
    let fa = eg.add(ENode::new(Op::Sqrt, vec![a]));
    let fb = eg.add(ENode::new(Op::Sqrt, vec![b]));
    let gfa = eg.add(ENode::new(Op::Exp, vec![fa]));
    let gfb = eg.add(ENode::new(Op::Exp, vec![fb]));
    eg.rebuild();
    assert!(!eg.equivalent(fa, fb));

    eg.union(a, b);
    eg.rebuild();
    eg.check_invariants();
    assert!(
        eg.equivalent(fa, fb),
        "congruence did not reach one level up"
    );
    assert!(
        eg.equivalent(gfa, gfb),
        "congruence did not reach two levels up"
    );
}

#[test]
fn congruence_propagates_through_every_argument_position() {
    let mut eg = empty();
    let a = eg.add(ENode::var("a"));
    let b = eg.add(ENode::var("b"));
    let c = eg.add(ENode::var("c"));
    let left = eg.add(ENode::new(Op::Div, vec![c, a]));
    let right = eg.add(ENode::new(Op::Div, vec![c, b]));
    eg.rebuild();
    eg.union(a, b);
    eg.rebuild();
    eg.check_invariants();
    assert!(eg.equivalent(left, right));
}

#[test]
fn unioning_everything_collapses_to_one_class() {
    let mut eg = empty();
    let e = parse("a + b * c - d / e").unwrap();
    let root = eg.add_expr(&e);
    eg.rebuild();
    let ids = eg.class_ids();
    for &id in &ids {
        eg.union(root, id);
    }
    eg.rebuild();
    eg.check_invariants();
    assert_eq!(eg.number_of_classes(), 1);
}

#[test]
fn random_unions_preserve_every_invariant() {
    // The invariants are subtle enough that hand-written cases will not find
    // the bad interleavings; this drives the graph with arbitrary unions and
    // checks after each batch.
    let sources = [
        "a + b * c",
        "sqrt(x) / (y + 1)",
        "exp(sin(t)) - exp(cos(t))",
        "min(p, q) * max(p, q)",
        "if(a < b, a * a, b * b)",
        "(u + v) ^ 3",
    ];
    for seed in 0..24u64 {
        let mut rng = Rng::seed(seed);
        let mut eg = empty();
        for src in sources {
            eg.add_expr(&parse(src).unwrap());
        }
        eg.rebuild();
        eg.check_invariants();

        for _ in 0..12 {
            let ids = eg.class_ids();
            if ids.len() < 2 {
                break;
            }
            for _ in 0..3 {
                let a = *rng.pick(&ids);
                let b = *rng.pick(&ids);
                eg.union(a, b);
            }
            eg.rebuild();
            eg.check_invariants();
        }
    }
}

#[test]
fn unions_are_order_independent() {
    // Whether a and b merge before or after c and d, the partition must end up
    // the same. A rebuild that lost work would show up here.
    let build = |order: &[(usize, usize)]| -> Vec<usize> {
        let mut eg = empty();
        let e = parse("f(a) + f(b) + f(c) + f(d)".replace("f", "sqrt").as_str()).unwrap();
        let root = eg.add_expr(&e);
        eg.rebuild();
        let leaves: Vec<Id> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| eg.lookup(&ENode::var(*n)).expect("leaf must be present"))
            .collect();
        for &(i, j) in order {
            eg.union(leaves[i], leaves[j]);
            eg.rebuild();
        }
        eg.check_invariants();
        let _ = root;
        // Canonical partition: each leaf's representative, renumbered.
        let mut seen: Vec<Id> = Vec::new();
        leaves
            .iter()
            .map(|&l| {
                let r = eg.find(l);
                match seen.iter().position(|&x| x == r) {
                    Some(i) => i,
                    None => {
                        seen.push(r);
                        seen.len() - 1
                    }
                }
            })
            .collect()
    };
    assert_eq!(build(&[(0, 1), (2, 3)]), build(&[(2, 3), (0, 1)]));
    assert_eq!(build(&[(0, 1), (1, 2)]), build(&[(1, 2), (0, 1)]));
}

#[test]
fn lookup_finds_exactly_what_was_added() {
    let mut eg = empty();
    let e = parse("a * b + 3").unwrap();
    eg.add_expr(&e);
    eg.rebuild();
    let a = eg.lookup(&ENode::var("a")).unwrap();
    let b = eg.lookup(&ENode::var("b")).unwrap();
    assert!(eg.lookup_op(Op::Mul, vec![a, b]).is_some());
    assert!(eg.lookup_op(Op::Div, vec![a, b]).is_none());
    assert!(eg.lookup(&ENode::var("zzz")).is_none());
    // Commutativity is structural, so the mirrored lookup must hit too.
    assert_eq!(
        eg.lookup_op(Op::Mul, vec![a, b]),
        eg.lookup_op(Op::Mul, vec![b, a])
    );
}

#[test]
fn constant_folding_reaches_through_a_union() {
    // `y` is not a constant, but proving `y = 2` should fold `x` where
    // `x = y * 3`, which only works if the analysis re-runs after the union.
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let e = parse("y * 3").unwrap();
    let root = eg.add_expr(&e);
    let two = eg.add_constant(2.0);
    let y = eg.lookup(&ENode::var("y")).unwrap();
    eg.rebuild();
    assert_eq!(eg[root].data.value(), None);

    eg.union(y, two);
    eg.rebuild();
    eg.check_invariants();
    assert_eq!(eg[root].data.value(), Some(6.0));
    let (_, best) = Extractor::new(&eg, AstSize).find_best(root);
    assert_eq!(best.to_sexp(), "6");
}

#[test]
fn analysis_survives_a_deep_chain() {
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let mut id = eg.add_constant(1.0);
    for _ in 0..200 {
        let one = eg.add_constant(1.0);
        id = eg.add(ENode::new(Op::Add, vec![id, one]));
    }
    eg.rebuild();
    eg.check_invariants();
    assert_eq!(eg[id].data.value(), Some(201.0));
}

#[test]
fn extraction_prefers_the_cheaper_of_two_equal_terms() {
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let cheap = eg.add_expr(&parse("a + a").unwrap());
    let dear = eg.add_expr(&parse("exp(ln(a)) * 2").unwrap());
    eg.union(cheap, dear);
    eg.rebuild();
    eg.check_invariants();
    let (_, best) = Extractor::new(&eg, OpCost).find_best(cheap);
    assert_eq!(best.to_sexp(), "(+ a a)");
}

#[test]
fn extraction_keeps_sharing() {
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let e = parse("let t = sin(x) * cos(x) in t + t * t").unwrap();
    let root = eg.add_expr(&e);
    eg.rebuild();
    let (_, best) = Extractor::new(&eg, AstSize).find_best(root);
    assert!(
        best.dag_size() < best.tree_size() as usize,
        "extraction expanded the DAG into a tree"
    );
}

#[test]
fn extraction_survives_a_cycle() {
    // `x` is unioned with `x + 0`, so the class refers to itself. Extraction
    // must still terminate and return the finite term.
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let x = eg.add(ENode::var("x"));
    let zero = eg.add_constant(0.0);
    let sum = eg.add(ENode::new(Op::Add, vec![x, zero]));
    eg.union(x, sum);
    eg.rebuild();
    eg.check_invariants();
    let (_, best) = Extractor::new(&eg, AstSize).find_best(x);
    assert_eq!(best.to_sexp(), "x");
}

#[test]
fn a_shared_dag_does_not_explode_in_the_graph() {
    // The printed form of this expression has 2^10 leaves; the e-graph must
    // stay linear in the DAG.
    let src = "let a = x + 1 in let b = a * a in let c = b * b in \
               let p = c * c in let q = p * p in q * q";
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let e = parse(src).unwrap();
    eg.add_expr(&e);
    eg.rebuild();
    eg.check_invariants();
    assert!(eg.total_nodes() < 20, "{} nodes", eg.total_nodes());
    assert!(e.tree_size() > 100, "tree is only {}", e.tree_size());
}

#[test]
fn dump_and_dot_are_well_formed() {
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    eg.add_expr(&parse("a * b + 1").unwrap());
    eg.rebuild();
    let dump = eg.dump();
    assert_eq!(dump.lines().count(), eg.number_of_classes());
    let dot = eg.to_dot();
    assert!(dot.starts_with("digraph"));
    assert!(dot.trim_end().ends_with('}'));
    assert_eq!(
        dot.matches("subgraph cluster_").count(),
        eg.number_of_classes()
    );
}

#[test]
#[should_panic(expected = "rebuild")]
fn checking_a_dirty_graph_is_rejected() {
    let mut eg = empty();
    let a = eg.add(ENode::var("a"));
    let b = eg.add(ENode::var("b"));
    eg.rebuild();
    eg.union(a, b);
    eg.check_invariants();
}
