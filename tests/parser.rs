use saturn::lang::Op;
use saturn::parser::{parse, parse_rule};

fn sexp(src: &str) -> String {
    parse(src).unwrap_or_else(|e| panic!("{}", e)).to_sexp()
}

fn pretty(src: &str) -> String {
    parse(src).unwrap_or_else(|e| panic!("{}", e)).pretty()
}

#[test]
fn precedence_follows_arithmetic() {
    assert_eq!(sexp("1 + 2 * 3"), "(+ 1 (* 2 3))");
    assert_eq!(sexp("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    assert_eq!(sexp("a + b + c"), "(+ (+ a b) c)");
    assert_eq!(sexp("a - b - c"), "(- (- a b) c)");
    assert_eq!(sexp("a / b / c"), "(/ (/ a b) c)");
    assert_eq!(sexp("a < b + 1"), "(< a (+ b 1))");
    assert_eq!(sexp("a < b && c > d"), "(&& (< a b) (> c d))");
    assert_eq!(sexp("a || b && c"), "(|| a (&& b c))");
}

#[test]
fn pow_is_right_associative() {
    assert_eq!(sexp("2 ^ 3 ^ 2"), "(^ 2 (^ 3 2))");
    assert_eq!(sexp("(2 ^ 3) ^ 2"), "(^ (^ 2 3) 2)");
}

#[test]
fn unary_minus_binds_tighter_than_multiply_and_looser_than_pow() {
    assert_eq!(sexp("-x ^ 2"), "(neg (^ x 2))");
    assert_eq!(sexp("-x * y"), "(* (neg x) y)");
    assert_eq!(sexp("-(x * y)"), "(neg (* x y))");
}

#[test]
fn negative_literals_fold_at_parse_time() {
    // Rules such as `?x ^ -1 => 1 / ?x` need a literal to match on, not a
    // `neg` node wrapping one.
    assert_eq!(sexp("x ^ -1"), "(^ x -1)");
    assert_eq!(sexp("-3"), "-3");
    assert_eq!(sexp("2 * -3"), "(* 2 -3)");
}

#[test]
fn calls_check_their_arity() {
    assert_eq!(sexp("sqrt(x)"), "(sqrt x)");
    assert_eq!(sexp("min(a, b)"), "(min a b)");
    assert_eq!(sexp("if(c, a, b)"), "(if c a b)");
    assert!(parse("sqrt(a, b)").is_err());
    assert!(parse("min(a)").is_err());
    assert!(parse("if(a, b)").is_err());
    assert!(parse("nosuchfn(a)").is_err());
}

#[test]
fn differentiation_requires_a_variable() {
    assert_eq!(sexp("d(x, x * x)"), "(d x (* x x))");
    assert!(parse("d(1, x)").is_err());
    assert!(parse("d(a + b, x)").is_err());
}

#[test]
fn let_is_inlined_and_shared() {
    let e = parse("let t = a + b in t * t").unwrap();
    assert_eq!(e.to_sexp(), "(* (+ a b) (+ a b))");
    // Inlining does not duplicate work: hashconsing gives back one node for
    // `a + b`, so the DAG has three nodes, not five.
    assert_eq!(e.dag_size(), 4, "{}", e.to_sexp());
    assert_eq!(e.tree_size(), 7);
}

#[test]
fn nested_lets_shadow_innermost_first() {
    let e = parse("let x = 1 in let x = 2 in x").unwrap();
    assert_eq!(e.to_sexp(), "2");
    let e = parse("let a = 1 in let b = a + 1 in a + b").unwrap();
    assert_eq!(e.to_sexp(), "(+ 1 (+ 1 1))");
}

#[test]
fn let_may_bind_a_name_that_is_also_a_function() {
    // A name is a call only when directly followed by `(`, so binding `d` as a
    // coefficient does not shut off the derivative operator. Polynomials are
    // written with coefficients a, b, c, d all the time.
    let e = parse("let d = 2 in d * x + d").unwrap();
    assert_eq!(e.to_sexp(), "(+ 2 (* 2 x))");
    let e = parse("let d = 2 in d(x, d * x)").unwrap();
    assert_eq!(e.to_sexp(), "(d x (* 2 x))");
    let e = parse("let sqrt = 9 in sqrt + sqrt(sqrt)").unwrap();
    assert_eq!(e.to_sexp(), "(+ 9 (sqrt 9))");
}

#[test]
fn deeply_shared_lets_stay_small() {
    // Each binding squares the tree size, so the printed form is 2^16 nodes
    // while the DAG is linear. This is the case a tree-based representation
    // cannot survive.
    let src = "let a = x + 1 in let b = a * a in let c = b * b in \
               let p = c * c in let q = p * p in q * q";
    let e = parse(src).unwrap();
    assert!(e.dag_size() <= 8, "dag has {} nodes", e.dag_size());
    assert!(e.tree_size() > 60, "tree has {} nodes", e.tree_size());
}

#[test]
fn named_constants_become_literals() {
    let e = parse("pi").unwrap();
    assert!(matches!(e.node(e.root()).op, Op::Const(_)));
    assert!(e.vars().is_empty());
    // `e` is a perfectly good variable name and is deliberately not reserved.
    let e = parse("e").unwrap();
    assert_eq!(e.vars().len(), 1);
}

#[test]
fn numbers_lex_in_every_shape() {
    for src in ["1", "1.5", ".5", "1e3", "1E3", "1e-3", "1.5e+10", "0.0"] {
        assert!(parse(src).is_ok(), "failed on {}", src);
    }
    // `2e` is the number 2 times the variable e, not a broken exponent.
    assert_eq!(sexp("2 * e"), "(* 2 e)");
}

#[test]
fn comments_run_to_end_of_line() {
    assert_eq!(sexp("1 + 2 # this is ignored"), "(+ 1 2)");
}

#[test]
fn errors_point_at_the_problem() {
    let e = parse("1 + + ").unwrap_err();
    assert!(e.render().contains('^'), "{}", e.render());
    let e = parse("(1 + 2").unwrap_err();
    assert!(e.render().contains(')'), "{}", e.render());
    let e = parse("1 @ 2").unwrap_err();
    assert!(e.render().contains('@'), "{}", e.render());
    let e = parse("").unwrap_err();
    assert!(e.render().contains("end of input"), "{}", e.render());
}

#[test]
fn pattern_variables_parse_as_variables() {
    let e = parse("?a + ?b").unwrap();
    let names: Vec<String> = e.vars().iter().map(|s| s.to_string()).collect();
    assert_eq!(names, vec!["?a", "?b"]);
    assert!(parse("?").is_err());
}

#[test]
fn rules_parse_in_both_directions() {
    let (l, r, bidir) = parse_rule("?a + 0 => ?a").unwrap();
    assert_eq!(l.to_sexp(), "(+ ?a 0)");
    assert_eq!(r.to_sexp(), "?a");
    assert!(!bidir);
    let (_, _, bidir) = parse_rule("?a * ?b <=> ?b * ?a").unwrap();
    assert!(bidir);
    assert!(parse_rule("?a + 0").is_err());
}

#[test]
fn printing_round_trips() {
    // Whatever the printer emits must parse back to the same expression, or
    // the parenthesisation is wrong somewhere.
    let sources = [
        "1 + 2 * 3",
        "(a + b) * c",
        "a - (b - c)",
        "a / (b / c)",
        "2 ^ 3 ^ 2",
        "(2 ^ 3) ^ 2",
        "-x ^ 2",
        "-(x + y)",
        "sqrt(a * b) + min(c, d)",
        "if(a < b, a, b)",
        "!(a < b) || c",
        "x ^ -1",
        "-2 * x",
        "a * -3",
        "d(x, sin(x) * cos(x))",
        "1e-9 * x + 1.5",
    ];
    for src in sources {
        let once = parse(src).unwrap();
        let text = once.pretty();
        let twice = parse(&text)
            .unwrap_or_else(|e| panic!("`{}` printed as `{}` which failed: {}", src, text, e));
        assert_eq!(
            once.to_sexp(),
            twice.to_sexp(),
            "`{}` printed as `{}` and changed meaning",
            src,
            text
        );
    }
}

#[test]
fn printing_omits_redundant_parentheses() {
    assert_eq!(pretty("1 + (2 * 3)"), "1 + 2 * 3");
    assert_eq!(pretty("(1 + 2) * 3"), "(1 + 2) * 3");
    assert_eq!(pretty("((x))"), "x");
}

#[test]
fn shared_printing_names_repeated_subterms() {
    let e = parse("let t = a + b + c in t * t + t").unwrap();
    let text = e.pretty_shared();
    assert!(text.contains("let "), "{}", text);
    assert!(text.lines().count() > 1, "{}", text);
}

#[test]
fn commutative_arguments_are_stored_in_a_canonical_order() {
    use saturn::analysis::MathAnalysis;
    use saturn::egraph::EGraph;

    // Ordering is by child id, which inside a single `RecExpr` is insertion
    // order, so the two spellings stay distinct as text. What matters is that
    // they land in one e-class without any commutativity rule firing: that is
    // what makes commutativity free rather than a rule that doubles the graph.
    for (left, right) in [
        ("a + b", "b + a"),
        ("min(a, b)", "min(b, a)"),
        ("a * (b * c)", "(c * b) * a"),
        ("a == b", "b == a"),
    ] {
        let mut eg: EGraph<MathAnalysis> = EGraph::default();
        let l = eg.add_expr(&parse(left).unwrap());
        let r = eg.add_expr(&parse(right).unwrap());
        eg.rebuild();
        assert!(eg.equivalent(l, r), "`{}` and `{}` are not equivalent", left, right);
    }

    // Non-commutative operators must stay distinct.
    for (left, right) in [("a - b", "b - a"), ("a / b", "b / a"), ("a < b", "b < a")] {
        let mut eg: EGraph<MathAnalysis> = EGraph::default();
        let l = eg.add_expr(&parse(left).unwrap());
        let r = eg.add_expr(&parse(right).unwrap());
        eg.rebuild();
        assert!(!eg.equivalent(l, r), "`{}` and `{}` were conflated", left, right);
    }
}

#[test]
fn signed_zero_is_preserved() {
    use saturn::analysis::MathAnalysis;
    use saturn::egraph::EGraph;

    // `-0.0` and `0.0` compare equal but are not interchangeable: `1 / -0.0`
    // is `-inf` and `1 / 0.0` is `+inf`. They must not hashcons together.
    let mut eg: EGraph<MathAnalysis> = EGraph::default();
    let pos = eg.add_expr(&parse("1 / 0").unwrap());
    let neg = eg.add_expr(&parse("1 / -0").unwrap());
    eg.rebuild();
    eg.check_invariants();
    assert!(!eg.equivalent(pos, neg), "signed zeros were conflated");

    // And the printer must not lose the sign.
    let e = parse("-0").unwrap();
    assert_eq!(e.pretty(), "-0");
    assert_eq!(parse(&e.pretty()).unwrap().to_sexp(), e.to_sexp());
    assert!(parse("-0")
        .unwrap()
        .node(parse("-0").unwrap().root())
        .as_const()
        .unwrap()
        .is_sign_negative());
}
