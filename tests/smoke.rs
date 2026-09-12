use saturn::*;

fn rules() -> Vec<Rewrite<MathAnalysis>> {
    vec![
        rw!("assoc-add"; "(?a + ?b) + ?c" => "?a + (?b + ?c)"),
        rw!("assoc-mul"; "(?a * ?b) * ?c" => "?a * (?b * ?c)"),
        rw!("add-0"; "?a + 0" => "?a"),
        rw!("mul-1"; "?a * 1" => "?a"),
        rw!("mul-0"; "?a * 0" => "0"),
        rw!("distribute"; "?a * (?b + ?c)" => "?a * ?b + ?a * ?c"),
        rw!("factor"; "?a * ?b + ?a * ?c" => "?a * (?b + ?c)"),
        rw!("sub-to-add"; "?a - ?b" => "?a + -1 * ?b"),
    ]
}

/// These tests run in debug builds, where the engine is roughly fifty times
/// slower than release. Bounding the work keeps the suite quick without
/// changing what any of them prove.
fn small_runner(e: &RecExpr) -> Runner<MathAnalysis> {
    Runner::default()
        .with_expr(e)
        .with_iter_limit(10)
        .with_node_limit(5_000)
        .with_time_limit(std::time::Duration::from_secs(5))
}

#[test]
fn parses_and_prints() {
    let e = parse("2 * x + 1").unwrap();
    assert_eq!(e.to_sexp(), "(+ (* 2 x) 1)");
    assert_eq!(e.pretty(), "2 * x + 1");
}

#[test]
fn egraph_invariants_hold() {
    let e = parse("(a + b) * (a + b) - (a + b)").unwrap();
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let root = eg.add_expr(&e);
    eg.rebuild();
    eg.check_invariants();

    let r = small_runner(&e).with_iter_limit(6).run(&rules());
    r.egraph.check_invariants();
    assert!(r.egraph.number_of_classes() > 0);
    let _ = root;
}

#[test]
fn constant_folding_works() {
    let e = parse("2 * 3 + 4").unwrap();
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let root = eg.add_expr(&e);
    eg.rebuild();
    eg.check_invariants();
    assert_eq!(eg[root].data.value(), Some(10.0));
    let (_, best) = Extractor::new(&eg, AstSize).find_best(root);
    assert_eq!(best.to_sexp(), "10");
}

#[test]
fn saturation_simplifies() {
    let e = parse("x * (1 + 0) * 1").unwrap();
    let r = small_runner(&e).run(&rules());
    let (cost, best) = Extractor::new(&r.egraph, AstSize).find_best(r.root());
    assert_eq!(best.to_sexp(), "x", "got {} at cost {}", best.to_sexp(), cost);
}

#[test]
fn factoring_is_found() {
    // Only reachable if the rules can run *backwards* through distribution,
    // which is the whole point of saturating instead of rewriting in place.
    let e = parse("x * y + x * z").unwrap();
    let r = small_runner(&e).run(&rules());
    let (_, best) = Extractor::new(&r.egraph, OpCost).find_best(r.root());
    assert_eq!(best.to_sexp(), "(* x (+ y z))", "got {}", best.pretty());
}

#[test]
fn interval_analysis_proves_nonzero() {
    let e = parse("x * x + 1").unwrap();
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let root = eg.add_expr(&e);
    eg.rebuild();
    // x*x >= 0 for every real x, so x*x + 1 >= 1 > 0 — but x may be NaN or
    // infinite, so the analysis must not claim finiteness.
    assert!(eg[root].data.range.lo >= 1.0 || eg[root].data.range.nan);
}
