//! E-matching and rule application.

use saturn::analysis::MathAnalysis;
use saturn::egraph::EGraph;
use saturn::extract::{AstSize, Extractor};
use saturn::lang::ENode;
use saturn::parser::parse;
use saturn::pattern::Pattern;
use saturn::rewrite::Rewrite;
use saturn::rw;

fn graph(src: &str) -> (EGraph<MathAnalysis>, saturn::Id) {
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let root = eg.add_expr(&parse(src).unwrap());
    eg.rebuild();
    (eg, root)
}

fn count(pat: &str, src: &str) -> usize {
    let (eg, _) = graph(src);
    Pattern::parse(pat)
        .unwrap()
        .search(&eg)
        .iter()
        .map(|m| m.len())
        .sum()
}

#[test]
fn a_ground_pattern_matches_itself() {
    assert_eq!(count("a + b", "a + b"), 1);
    assert_eq!(count("a + c", "a + b"), 0);
    assert_eq!(count("a - b", "b - a"), 0);
}

#[test]
fn variables_bind_to_e_classes() {
    let (eg, _) = graph("x * y");
    let pat = Pattern::parse("?a * ?b").unwrap();
    let matches = pat.search(&eg);
    assert_eq!(matches.len(), 1);
    // Commutativity is structural, so `?a * ?b` matches a two-argument product
    // in both directions.
    assert_eq!(matches[0].len(), 2);
    for s in &matches[0].substs {
        assert_eq!(s.len(), 2);
    }
}

#[test]
fn a_repeated_variable_demands_the_same_class() {
    assert_eq!(count("?a - ?a", "x - x"), 1);
    assert_eq!(count("?a - ?a", "x - y"), 0);

    // ... and "the same class", not "the same spelling": once `x` and `y` are
    // proven equal, the pattern fires.
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    eg.add_expr(&parse("x - y").unwrap());
    eg.rebuild();
    let x = eg.lookup(&ENode::var("x")).unwrap();
    let y = eg.lookup(&ENode::var("y")).unwrap();
    eg.union(x, y);
    eg.rebuild();
    let n: usize = Pattern::parse("?a - ?a")
        .unwrap()
        .search(&eg)
        .iter()
        .map(|m| m.len())
        .sum();
    assert_eq!(n, 1);
}

#[test]
fn literals_in_a_pattern_must_match_exactly() {
    assert_eq!(count("?a + 0", "x + 0"), 1);
    assert_eq!(count("?a + 0", "x + 1"), 0);
    assert_eq!(count("?a ^ -1", "x ^ -1"), 1);
    assert_eq!(count("?a ^ -1", "x ^ 1"), 0);
}

#[test]
fn nested_patterns_match_at_every_depth() {
    assert_eq!(count("?a * (?b + ?c)", "x * (y + z)"), 2);
    assert_eq!(count("sqrt(?a)", "sqrt(sqrt(x))"), 2);
    assert_eq!(count("?a + ?b", "a + b + c"), 4);
}

#[test]
fn a_bare_variable_is_rejected_as_a_left_hand_side() {
    let r: Result<Rewrite<MathAnalysis>, String> = Rewrite::parse("bad", "?a => ?a + 0");
    assert!(
        r.is_err(),
        "a pattern that matches every class was accepted"
    );
    assert!(r.unwrap_err().contains("matches everything"));
}

#[test]
fn an_unbound_right_hand_variable_is_rejected() {
    let r: Result<Rewrite<MathAnalysis>, String> = Rewrite::parse("bad", "?a + 0 => ?a * ?k");
    assert!(r.is_err(), "an unbound right-hand variable was accepted");
    assert!(r.unwrap_err().contains("not bound"));
}

#[test]
fn applying_a_rule_adds_an_equality_without_removing_anything() {
    let (mut eg, root) = graph("x * 1");
    let rule: Rewrite<MathAnalysis> = rw!("mul-1"; "?a * 1" => "?a");
    let matches = rule.search(&eg);
    assert_eq!(matches.len(), 1);
    rule.apply(&mut eg, &matches);
    eg.rebuild();
    eg.check_invariants();

    let x = eg.lookup(&ENode::var("x")).unwrap();
    assert!(eg.equivalent(root, x));
    // The original term is still there; nothing is ever destroyed.
    assert!(eg[root].nodes.iter().any(|n| n.op == saturn::Op::Mul));
}

#[test]
fn a_conditional_rule_fires_only_when_its_condition_holds() {
    use saturn::rules::is_finite_nonzero;
    let rule: Rewrite<MathAnalysis> = Rewrite::new(
        "cancel",
        Pattern::parse("?a / ?a").unwrap(),
        Box::new(saturn::rewrite::ConditionalApplier {
            condition: is_finite_nonzero("?a"),
            description: "?a is finite and nonzero".into(),
            applier: Box::new(Pattern::parse("1").unwrap()),
        }),
    )
    .unwrap();

    // `x` could be zero, infinite or NaN, so cancelling is wrong.
    let (mut eg, root) = graph("x / x");
    let ms = rule.search(&eg);
    assert_eq!(ms.iter().map(|m| m.len()).sum::<usize>(), 1);
    rule.apply(&mut eg, &ms);
    eg.rebuild();
    let (_, best) = Extractor::new(&eg, AstSize).find_best(root);
    assert_eq!(
        best.to_sexp(),
        "(/ x x)",
        "cancelled an unprovable division"
    );

    // `3` is finite and nonzero, so it cancels.
    let (mut eg, root) = graph("(2 + 1) / (2 + 1)");
    let ms = rule.search(&eg);
    rule.apply(&mut eg, &ms);
    eg.rebuild();
    let (_, best) = Extractor::new(&eg, AstSize).find_best(root);
    assert_eq!(best.to_sexp(), "1");
}

#[test]
fn a_dynamic_applier_can_read_the_match() {
    use saturn::lang::{ENode as N, Op};
    use saturn::rewrite::DynamicApplier;
    use saturn::sym::Sym;

    // Expand `?x ^ k` into a multiplication chain for a small integral k --
    // something no fixed pattern can express, because k is only known when
    // the match happens.
    let rule: Rewrite<MathAnalysis> = Rewrite::new(
        "expand-pow",
        Pattern::parse("?x ^ ?k").unwrap(),
        Box::new(DynamicApplier {
            description: "?x * ?x * ... (k times)".into(),
            f: Box::new(|eg: &mut EGraph<MathAnalysis>, matched, subst| {
                let (x, k) = (Sym::new("?x"), Sym::new("?k"));
                let (Some(xc), Some(kc)) = (subst.get(x), subst.get(k)) else {
                    return Vec::new();
                };
                let Some(n) = eg[kc].data.value() else {
                    return Vec::new();
                };
                if n != n.trunc() || !(2.0..=8.0).contains(&n) {
                    return Vec::new();
                }
                let mut acc = xc;
                for _ in 1..(n as i64) {
                    acc = eg.add(N::new(Op::Mul, vec![acc, xc]));
                }
                if eg.union(matched, acc) {
                    vec![acc]
                } else {
                    Vec::new()
                }
            }),
        }),
    )
    .unwrap();

    let (mut eg, root) = graph("x ^ 3");
    let ms = rule.search(&eg);
    rule.apply(&mut eg, &ms);
    eg.rebuild();
    eg.check_invariants();
    let (_, best) = Extractor::new(&eg, saturn::extract::OpCost).find_best(root);
    // Commutative children are stored in canonical id order, so the chain
    // prints right-nested regardless of how it was built.
    assert_eq!(best.to_sexp(), "(* x (* x x))");

    // A non-integral exponent leaves the term alone.
    let (mut eg, root) = graph("x ^ 0.5");
    let ms = rule.search(&eg);
    rule.apply(&mut eg, &ms);
    eg.rebuild();
    let (_, best) = Extractor::new(&eg, AstSize).find_best(root);
    assert_eq!(best.to_sexp(), "(^ x 0.5)");
}

#[test]
fn searching_is_deterministic() {
    let (eg, _) = graph("(a + b) * (a + b) + a * b");
    let pat = Pattern::parse("?x * ?y").unwrap();
    let once: Vec<String> = pat
        .search(&eg)
        .iter()
        .map(|m| format!("{:?} {:?}", m.eclass, m.substs))
        .collect();
    for _ in 0..8 {
        let again: Vec<String> = pat
            .search(&eg)
            .iter()
            .map(|m| format!("{:?} {:?}", m.eclass, m.substs))
            .collect();
        assert_eq!(once, again, "e-matching is not reproducible");
    }
}

#[test]
fn instantiation_reuses_matched_classes() {
    let (mut eg, _) = graph("x + y");
    let before = eg.number_of_classes();
    let pat = Pattern::parse("?a + ?b").unwrap();
    let ms = pat.search(&eg);
    let subst = &ms[0].substs[0];
    // Rebuilding the very pattern that matched must not create a single class.
    let id = pat.instantiate(&mut eg, subst);
    eg.rebuild();
    assert_eq!(eg.number_of_classes(), before);
    assert!(eg.equivalent(id, ms[0].eclass));
}

#[test]
fn bidirectional_rules_expand_to_two() {
    let rules: Vec<Rewrite<MathAnalysis>> =
        Rewrite::parse_bidirectional("assoc", "(?a + ?b) + ?c <=> ?a + (?b + ?c)").unwrap();
    assert_eq!(rules.len(), 2);
    assert!(rules[0].name.ends_with("-fwd"));
    assert!(rules[1].name.ends_with("-rev"));
    assert_eq!(rules[0].searcher.to_string(), rules[1].applier.describe());
}

#[test]
fn rules_print_readably() {
    let r: Rewrite<MathAnalysis> = rw!("mul-1"; "?a * 1" => "?a");
    assert_eq!(r.long_name(), "(?a * 1) => ?a");
    assert!(r.to_string().starts_with("mul-1:"));
}
