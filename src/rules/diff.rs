//! Symbolic differentiation, expressed as rewrite rules.
//!
//! `d(x, e)` is an ordinary node of the language: child 0 is the
//! [`Op::Var`](crate::Op::Var) naming the variable, child 1 is the body. The rules below push the
//! derivative towards the leaves until no `d` is left.
//! [`OpCost`](crate::OpCost) prices [`Op::Diff`](crate::Op::Diff) out of
//! reach, so a `d` these rules could not eliminate shows up as an absurd
//! extraction cost rather than as a silently wrong answer.
//!
//! In an ordinary term rewriter, differentiation is two passes: differentiate,
//! then clean up the blizzard of `1 *`, `+ 0` and `0 *` that the chain rule
//! leaves behind. That order is also a commitment — a derivative already
//! rearranged by the simplifier can no longer be rearranged a different way.
//! Here there is no order. A derivative rule and an algebraic rule are the
//! same kind of object running in the same saturation loop, so the raw
//! product-rule form of `d(x, x*x)`, the `x + x` the simplifier makes of it,
//! and the `2 * x` some other rule makes of *that* all land in one e-class,
//! and the cost model chooses between them at the end. Differentiating inside
//! an e-graph means the derivative and its simplification happen
//! simultaneously rather than in sequence; which simplification rules are
//! available is the caller's choice, since this module supplies only the ones
//! that eliminate `d`.
//!
//! Every rule here is exact over the reals. The piecewise operators — `abs`,
//! `min`, `max`, `sign`, `floor`, `ceil`, `if`, and the comparisons — are
//! differentiated *almost everywhere*: the identities hold off the measure-zero
//! set of kinks and jumps, where no derivative exists to be right about.
//!
//! One of the rules is not really a pattern. `d(x, e) => 0` has to know
//! whether `e` can vary with `x`, which no left-hand side can ask; the
//! side condition searches the e-graph for the answer, and `free_of_var`
//! below is where that subtlety lives.

use super::{and, Cond, Rule};
use crate::analysis::MathAnalysis;
use crate::egraph::EGraph;
use crate::lang::Id;
use crate::rw;
use crate::sym::Sym;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Deciding whether a body can vary with the variable
// ---------------------------------------------------------------------------

/// The variables that `d(v, _)` might be differentiating with respect to,
/// given that `v` is bound to `class`.
///
/// The parser insists the first argument of `d` is written as a variable, but
/// an e-class is a *set* of nodes: some rule may since have proven that
/// variable equal to another term, leaving the class with several nodes or
/// several variables. Callers therefore demand their answer hold for every
/// variable in the class, and refuse to answer at all when there is none.
fn differentiation_vars(egraph: &EGraph<MathAnalysis>, class: Id) -> Vec<Sym> {
    let mut vars: Vec<Sym> = egraph[class].iter().filter_map(|n| n.as_var()).collect();
    vars.sort();
    vars.dedup();
    vars
}

/// Does `class` provably denote a value that `var` cannot affect?
///
/// This is what `d(x, e) => 0` rests on, and getting its direction right is
/// the whole difficulty of differentiating inside an e-graph. The tempting
/// question — "is a `Var(var)` node reachable from this class?" — is the wrong
/// one, and answering `false` to it is not the conservative choice it looks
/// like. Every class holds *equivalent* nodes, so one x-free node in it is a
/// proof that the class's value is x-free no matter what the other nodes look
/// like. Insisting that no node anywhere mention `x` is not merely imprecise,
/// it is self-defeating: `d(x, x) => 1` drops a term that mentions `x` into
/// the class of the literal `1`, and from then on no `d(x, 1)` anywhere in the
/// graph could ever be reduced — which is exactly the `d(x, 1)` the quotient
/// and product rules manufacture.
///
/// So the question asked here is whether the class can denote *some* term free
/// of `var`, and the answer is a witness rather than an absence. That makes it
/// a least fixpoint: a class is free of `var` as soon as it holds one node
/// that is not `Var(var)` and whose every child class is already known free,
/// and the search grows outwards from the leaves until nothing more can be
/// admitted. Cycles — union `y` with `y * 1` and the resulting class contains
/// a node pointing back at itself — cost nothing here, because a cyclic node
/// simply never becomes a witness and the worklist drains anyway.
///
/// The one thing this leans on is that the e-graph's unions are real
/// identities. They are what makes a single witness speak for the whole class:
/// if two nodes denote the same function and one of them cannot see `var`,
/// neither can the value. An unsound rule that equates two genuinely different
/// functions breaks that, but it has already broken far more than this.
fn free_of_var(egraph: &EGraph<MathAnalysis>, class: Id, var: Sym) -> bool {
    let root = egraph.find(class);

    // Everything the body can reach. Children of anything in here are in here
    // too, so the fixpoint below never has to look outside it.
    let mut cone: Vec<Id> = vec![root];
    let mut users: HashMap<Id, Vec<Id>> = HashMap::new();
    let mut seen: HashSet<Id> = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        for node in egraph[id].iter() {
            for &child in node.children() {
                let child = egraph.find(child);
                users.entry(child).or_default().push(id);
                if seen.insert(child) {
                    cone.push(child);
                    stack.push(child);
                }
            }
        }
    }
    for parents in users.values_mut() {
        parents.sort_unstable();
        parents.dedup();
    }

    let witnessed = |id: Id, free: &HashSet<Id>| {
        egraph[id].iter().any(|n| {
            n.as_var() != Some(var) && n.children().iter().all(|&c| free.contains(&egraph.find(c)))
        })
    };

    // Seeded with the leaves, since `all` over no children is vacuously true.
    let mut free: HashSet<Id> = HashSet::new();
    let mut queue: Vec<Id> = Vec::new();
    for &id in &cone {
        if witnessed(id, &free) {
            free.insert(id);
            queue.push(id);
        }
    }
    while let Some(id) = queue.pop() {
        if free.contains(&root) {
            return true;
        }
        for &user in users.get(&id).into_iter().flatten() {
            if !free.contains(&user) && witnessed(user, &free) {
                free.insert(user);
                queue.push(user);
            }
        }
    }
    free.contains(&root)
}

/// `?body` denotes a value that the variable bound to `?var` cannot affect.
fn independent_of(body: &str, var: &str) -> Cond {
    let (body, var) = (Sym::new(body), Sym::new(var));
    Box::new(move |egraph, _matched, subst| {
        let (Some(b), Some(v)) = (subst.get(body), subst.get(var)) else {
            return false;
        };
        let vars = differentiation_vars(egraph, v);
        !vars.is_empty() && vars.iter().all(|&s| free_of_var(egraph, b, s))
    })
}

/// `?body` is not provably free of the variable bound to `?var`.
///
/// Whenever there is a variable to differentiate by this is the exact
/// complement of [`independent_of`], which is what keeps the two power rules
/// below from ever both firing on one match and unioning two derivatives that
/// disagree.
fn depends_on(body: &str, var: &str) -> Cond {
    let (body, var) = (Sym::new(body), Sym::new(var));
    Box::new(move |egraph, _matched, subst| {
        let (Some(b), Some(v)) = (subst.get(body), subst.get(var)) else {
            return false;
        };
        let vars = differentiation_vars(egraph, v);
        vars.iter().any(|&s| !free_of_var(egraph, b, s))
    })
}

/// `?v` is not the literal zero.
fn not_literal_zero(v: &str) -> Cond {
    let sym = Sym::new(v);
    Box::new(move |egraph, _matched, subst| match subst.get(sym) {
        Some(id) => egraph[id].data.value() != Some(0.0),
        None => false,
    })
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// Bodies whose value can only ever change by jumping, so that wherever a
/// derivative exists at all it is zero.
const LOCALLY_CONSTANT: &[(&str, &str)] = &[
    ("sign", "sign(?a)"),
    ("floor", "floor(?a)"),
    ("ceil", "ceil(?a)"),
    ("lt", "?a < ?b"),
    ("le", "?a <= ?b"),
    ("gt", "?a > ?b"),
    ("ge", "?a >= ?b"),
    ("eq", "?a == ?b"),
    ("ne", "?a != ?b"),
    ("and", "?a && ?b"),
    ("or", "?a || ?b"),
    ("not", "!?a"),
];

/// The differentiation rules: everything needed to eliminate `d(x, e)`.
pub fn safe() -> Vec<Rule> {
    let mut rules = vec![
        // `?x` appearing twice forces both e-classes to be the same one, so
        // this fires exactly when the body *is* the variable.
        rw!("diff-var"; "d(?x, ?x)" => "1"),
        rw!("diff-independent"; "d(?x, ?e)" => "0",
            if "?e is provably free of ?x", independent_of("?e", "?x")),
        rw!("diff-add"; "d(?x, ?a + ?b)" => "d(?x, ?a) + d(?x, ?b)"),
        rw!("diff-sub"; "d(?x, ?a - ?b)" => "d(?x, ?a) - d(?x, ?b)"),
        rw!("diff-neg"; "d(?x, -?a)" => "-d(?x, ?a)"),
        rw!("diff-mul"; "d(?x, ?a * ?b)" => "d(?x, ?a) * ?b + ?a * d(?x, ?b)"),
        rw!("diff-div"; "d(?x, ?a / ?b)"
            => "(d(?x, ?a) * ?b - ?a * d(?x, ?b)) / (?b * ?b)"),
        // `?f ^ 0` is the constant function 1 for *every* `?f` — IEEE-754
        // makes even `0 ^ 0` and `nan ^ 0` equal 1 — so this case is split out
        // rather than left to the rule below, which would produce
        // `0 * ?f ^ -1` and so NaN rather than 0 at `?f = 0`.
        rw!("diff-pow-zero"; "d(?x, ?f ^ 0)" => "0"),
        rw!("diff-pow-const"; "d(?x, ?f ^ ?g)" => "?g * ?f ^ (?g - 1) * d(?x, ?f)",
            if "?g is a nonzero exponent provably free of ?x",
            and(independent_of("?g", "?x"), not_literal_zero("?g"))),
        // The logarithmic form is the only one that handles a varying
        // exponent, and it is confined to that case. For `?f < 0` it yields
        // NaN through `ln(?f)`, which is not a loss: with `?g` varying,
        // `?f ^ ?g` is defined at negative `?f` only where `?g` happens to hit
        // an integer, a set with no interior and hence no derivative anywhere
        // on it. Where `?g` is fixed, `diff-pow-const` handles negative bases
        // exactly, and the two conditions are complementary so they never both
        // fire and union two disagreeing derivatives.
        rw!("diff-pow"; "d(?x, ?f ^ ?g)"
            => "?f ^ ?g * (d(?x, ?g) * ln(?f) + ?g * d(?x, ?f) / ?f)",
            if "?g is not provably free of ?x", depends_on("?g", "?x")),
        rw!("diff-sqrt"; "d(?x, sqrt(?a))" => "d(?x, ?a) / (2 * sqrt(?a))"),
        rw!("diff-exp"; "d(?x, exp(?a))" => "exp(?a) * d(?x, ?a)"),
        rw!("diff-ln"; "d(?x, ln(?a))" => "d(?x, ?a) / ?a"),
        rw!("diff-sin"; "d(?x, sin(?a))" => "cos(?a) * d(?x, ?a)"),
        rw!("diff-cos"; "d(?x, cos(?a))" => "-sin(?a) * d(?x, ?a)"),
        rw!("diff-tan"; "d(?x, tan(?a))" => "d(?x, ?a) / (cos(?a) * cos(?a))"),
        rw!("diff-atan2"; "d(?x, atan2(?a, ?b))"
            => "(?b * d(?x, ?a) - ?a * d(?x, ?b)) / (?a * ?a + ?b * ?b)"),
        // `sign` is the derivative of `abs` away from zero, and `sign(0) = 0`
        // is the symmetric choice at the kink — the same convention the
        // midpoint below takes for `min` and `max`.
        rw!("diff-abs"; "d(?x, abs(?a))" => "sign(?a) * d(?x, ?a)"),
        // `min` and `max` need care because their arguments are stored in a
        // canonical order and the matcher tries both, so this right-hand side
        // is instantiated once as written and once with `?a` and `?b` swapped;
        // both are unioned into the derivative's class and must therefore
        // agree on *every* input, not merely almost everywhere. The naive
        // `if(?a < ?b, d(?x, ?a), d(?x, ?b))` fails that test: on a tie it
        // yields `d(?x, ?b)` one way round and `d(?x, ?a)` the other. Deciding
        // the tie by a value symmetric in the two derivatives repairs it, and
        // the midpoint is the value differentiating
        // `min(a, b) = (a + b - abs(a - b)) / 2` gives under `sign(0) = 0`.
        // Comparisons against NaN are all false, so a NaN argument also lands
        // on the symmetric branch and the two instantiations still agree.
        rw!("diff-min"; "d(?x, min(?a, ?b))"
            => "if(?a < ?b, d(?x, ?a), \
                   if(?b < ?a, d(?x, ?b), (d(?x, ?a) + d(?x, ?b)) / 2))"),
        rw!("diff-max"; "d(?x, max(?a, ?b))"
            => "if(?a > ?b, d(?x, ?a), \
                   if(?b > ?a, d(?x, ?b), (d(?x, ?a) + d(?x, ?b)) / 2))"),
        // Sound wherever the condition is locally constant, which is
        // everywhere except the surface it switches on.
        rw!("diff-if"; "d(?x, if(?c, ?a, ?b))" => "if(?c, d(?x, ?a), d(?x, ?b))"),
    ];

    for (tag, body) in LOCALLY_CONSTANT {
        rules.push(rw!(format!("diff-{}", tag); &format!("d(?x, {})", body) => "0"));
    }

    rules
}

/// No rules.
///
/// The fast-math tier exists for identities that are true over the reals but
/// change a floating-point result. Differentiation has none to offer: a `d`
/// node has no floating-point result to change, and every rule that removes
/// one is already exact, so there is nothing here that would not belong in
/// [`safe`] instead.
pub fn fast_math() -> Vec<Rule> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egraph::EGraph;
    use crate::eval::{eval, Env};
    use crate::extract::{Extractor, OpCost};
    use crate::lang::{Op, RecExpr};
    use crate::parser::parse;
    use crate::rules::arith;
    use crate::runner::Runner;

    /// Stand-ins for the handful of identities the derivative tests rely on to
    /// reach a canonical shape. `arith::safe()` is the real home of these;
    /// they are repeated here so the assertions below pin down the
    /// differentiation rules and not the current contents of that module.
    fn cleanup() -> Vec<Rule> {
        vec![
            // Exact for every double, `-0.0`, infinities and NaN included.
            rw!("t-mul-1"; "?a * 1" => "?a"),
        ]
    }

    fn saturate(src: &str, rules: &[Rule]) -> RecExpr {
        let expr = parse(src).unwrap();
        let runner = Runner::default().with_expr(&expr).run(rules);
        let (_, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
        best
    }

    /// Differentiate with the rules of this module plus minimal cleanup.
    fn derivative(src: &str) -> RecExpr {
        let mut rules = safe();
        rules.extend(cleanup());
        saturate(src, &rules)
    }

    fn at(expr: &RecExpr, bindings: &[(&str, f64)]) -> f64 {
        let env: Env = bindings
            .iter()
            .map(|&(name, value)| (Sym::new(name), value))
            .collect();
        eval(expr, &env).unwrap()
    }

    fn assert_close(got: f64, want: f64) {
        assert!(
            (got - want).abs() <= 1e-9 * want.abs().max(1.0),
            "got {}, want {}",
            got,
            want
        );
    }

    fn assert_no_diff(expr: &RecExpr) {
        assert!(
            expr.nodes().iter().all(|n| n.op != Op::Diff),
            "a `d` node survived: {:?}",
            expr
        );
    }

    // -- the independence fixpoint ------------------------------------------

    fn graph_of(src: &str) -> (EGraph<MathAnalysis>, Id) {
        let mut egraph = EGraph::default();
        let root = egraph.add_expr(&parse(src).unwrap());
        egraph.rebuild();
        (egraph, root)
    }

    #[test]
    fn free_of_var_follows_the_body_to_its_leaves() {
        let (egraph, root) = graph_of("sin(x * y) + 3");
        assert!(!free_of_var(&egraph, root, Sym::new("x")));
        assert!(!free_of_var(&egraph, root, Sym::new("y")));
        assert!(free_of_var(&egraph, root, Sym::new("z")));
    }

    #[test]
    fn free_of_var_accepts_a_witness_hidden_among_the_alternatives() {
        // `x - x` is proven equal to `0`, which is the only spelling in the
        // class free of `x` — and one is enough, since all of them denote the
        // same value.
        let mut egraph: EGraph<MathAnalysis> = EGraph::default();
        let diff = egraph.add_expr(&parse("x - x").unwrap());
        let zero = egraph.add_constant(0.0);
        egraph.union(diff, zero);
        let body = egraph.add_expr(&parse("sin(x - x)").unwrap());
        egraph.rebuild();
        assert!(free_of_var(&egraph, body, Sym::new("x")));

        // Nothing was proven about `sin(x * y)`, so it stays unknown.
        let opaque = egraph.add_expr(&parse("sin(x * y)").unwrap());
        egraph.rebuild();
        assert!(!free_of_var(&egraph, opaque, Sym::new("x")));
    }

    #[test]
    fn free_of_var_terminates_on_a_cyclic_class() {
        let mut egraph: EGraph<MathAnalysis> = EGraph::default();
        let y = egraph.add_expr(&parse("y").unwrap());
        let scaled = egraph.add_expr(&parse("y * 1").unwrap());
        egraph.union(y, scaled);
        egraph.rebuild();

        let cyclic = egraph[y].iter().any(|n| {
            n.children()
                .iter()
                .any(|&c| egraph.find(c) == egraph.find(y))
        });
        assert!(cyclic, "the test needs a class that references itself");

        assert!(!free_of_var(&egraph, y, Sym::new("y")));
        assert!(free_of_var(&egraph, y, Sym::new("x")));
    }

    #[test]
    fn free_of_var_looks_through_a_nested_diff() {
        let (egraph, root) = graph_of("d(y, x * y)");
        assert!(!free_of_var(&egraph, root, Sym::new("x")));
        assert!(free_of_var(&egraph, root, Sym::new("w")));
    }

    #[test]
    fn differentiation_vars_reads_the_variable_out_of_its_class() {
        let (egraph, root) = graph_of("x");
        assert_eq!(differentiation_vars(&egraph, root), vec![Sym::new("x")]);
        let (egraph, root) = graph_of("x + 1");
        assert!(differentiation_vars(&egraph, root).is_empty());
    }

    // -- shapes -------------------------------------------------------------

    #[test]
    fn extracts_the_expected_shape() {
        for (src, want) in [
            ("d(x, x)", "1"),
            ("d(x, y)", "0"),
            ("d(x, 7)", "0"),
            ("d(x, x * x)", "x + x"),
            ("d(x, sin(x))", "cos(x)"),
            ("d(x, exp(x))", "exp(x)"),
            ("d(x, x ^ 3)", "3 * x ^ 2"),
            ("d(x, x ^ 0)", "0"),
            ("d(x, floor(x))", "0"),
            ("d(x, x > 1)", "0"),
        ] {
            let got = derivative(src);
            assert_eq!(got.pretty(), want, "differentiating {}", src);
        }
    }

    #[test]
    fn a_body_that_never_mentions_the_variable_differentiates_to_zero() {
        for src in ["d(x, y)", "d(x, sin(y) * 3)", "d(x, min(y, z) ^ w)"] {
            assert_eq!(derivative(src).pretty(), "0", "differentiating {}", src);
        }
    }

    // -- values -------------------------------------------------------------

    #[test]
    fn chain_and_quotient_rules_give_the_right_values() {
        // (source, x, expected derivative at x)
        let cases: &[(&str, f64, f64)] = &[
            ("d(x, 1 / x)", 2.0, -0.25),
            ("d(x, sqrt(x))", 4.0, 0.25),
            ("d(x, ln(x))", 2.0, 0.5),
            ("d(x, exp(2 * x))", 0.5, 2.0 * std::f64::consts::E),
            ("d(x, sin(x * x))", 1.0, 2.0 * 1.0f64.cos()),
            ("d(x, cos(x))", 0.7, -0.7f64.sin()),
            ("d(x, tan(x))", 0.3, 1.0 / (0.3f64.cos() * 0.3f64.cos())),
            ("d(x, abs(x))", -3.0, -1.0),
            ("d(x, -(x * x))", 3.0, -6.0),
            ("d(x, atan2(x, 1))", 0.0, 1.0),
            ("d(x, (x + 1) / (x - 1))", 3.0, -0.5),
        ];
        for &(src, x, want) in cases {
            let got = derivative(src);
            assert_no_diff(&got);
            assert_close(at(&got, &[("x", x)]), want);
        }
    }

    #[test]
    fn a_varying_exponent_uses_the_logarithmic_rule() {
        let got = derivative("d(x, x ^ x)");
        assert_no_diff(&got);
        // d/dx x^x = x^x (ln x + 1)
        assert_close(at(&got, &[("x", 2.0)]), 4.0 * (2.0f64.ln() + 1.0));
        assert_close(at(&got, &[("x", 0.5)]), 0.5f64.sqrt() * (0.5f64.ln() + 1.0));
    }

    #[test]
    fn min_max_and_if_pick_the_active_branch() {
        let min = derivative("d(x, min(x, 1))");
        assert_no_diff(&min);
        assert_close(at(&min, &[("x", 0.0)]), 1.0);
        assert_close(at(&min, &[("x", 2.0)]), 0.0);
        // At the tie the function has no derivative; the rule is symmetric in
        // its two arguments there rather than arbitrary.
        assert_close(at(&min, &[("x", 1.0)]), 0.5);

        let max = derivative("d(x, max(x, 1))");
        assert_no_diff(&max);
        assert_close(at(&max, &[("x", 2.0)]), 1.0);
        assert_close(at(&max, &[("x", 0.0)]), 0.0);

        let cond = derivative("d(x, if(x > 0, x * x, -x))");
        assert_no_diff(&cond);
        assert_close(at(&cond, &[("x", 2.0)]), 4.0);
        assert_close(at(&cond, &[("x", -3.0)]), -1.0);
    }

    #[test]
    fn a_nested_derivative_reduces_to_the_second_derivative() {
        let got = derivative("d(x, d(x, x * x))");
        assert_no_diff(&got);
        assert_close(at(&got, &[("x", 6.0)]), 2.0);
    }

    #[test]
    fn derivatives_agree_with_a_central_difference() {
        // Sample points stay clear of the kinks in `abs`, `min` and `max`,
        // where the rules are deliberately only right almost everywhere.
        let cases: &[(&str, &[f64])] = &[
            ("x * x * x", &[-2.0, 0.5, 3.0]),
            ("sin(x) * exp(x)", &[-1.0, 0.25, 2.0]),
            ("ln(x) + sqrt(x)", &[0.5, 3.0]),
            ("1 / (x + 2)", &[-1.0, 4.0]),
            ("(x + 1) / (x * x + 3)", &[-2.0, 0.5, 5.0]),
            ("x ^ 4", &[-1.5, 2.0]),
            ("x ^ x", &[0.4, 2.5]),
            ("tan(x)", &[-0.4, 0.9]),
            ("cos(x * x)", &[0.7, 1.6]),
            ("exp(sin(x))", &[-2.0, 1.0]),
            ("abs(x) * x", &[-3.0, 2.0]),
            ("min(x * x, 3)", &[0.5, 4.0]),
            ("max(x, 10)", &[2.0, 20.0]),
            ("atan2(x, 2)", &[-1.0, 3.0]),
            ("if(x > 1, x * x, x)", &[0.0, 4.0]),
        ];
        for &(body, points) in cases {
            let f = parse(body).unwrap();
            let got = derivative(&format!("d(x, {})", body));
            assert_no_diff(&got);
            for &x in points {
                let h = 1e-6 * x.abs().max(1.0);
                let numeric = (at(&f, &[("x", x + h)]) - at(&f, &[("x", x - h)])) / (2.0 * h);
                let exact = at(&got, &[("x", x)]);
                assert!(
                    (exact - numeric).abs() <= 1e-4 * numeric.abs().max(1.0),
                    "d/dx {} at {}: rules give {}, difference quotient gives {}",
                    body,
                    x,
                    exact,
                    numeric
                );
            }
        }
    }

    #[test]
    fn partial_derivatives_treat_other_variables_as_constants() {
        let got = derivative("d(x, x * y + y * y)");
        assert_no_diff(&got);
        assert_close(at(&got, &[("x", 4.0), ("y", 3.0)]), 3.0);
    }

    // -- interaction with the rest of the library ---------------------------

    #[test]
    fn simplifies_alongside_the_arithmetic_rules() {
        let mut rules = safe();
        rules.extend(arith::safe());
        rules.extend(cleanup());

        let got = saturate("d(x, x * x)", &rules);
        assert_no_diff(&got);
        for x in [-2.5, 0.0, 1.0, 7.0] {
            assert_close(at(&got, &[("x", x)]), 2.0 * x);
        }

        let got = saturate("d(x, x * x * x + 3 * x + 1)", &rules);
        assert_no_diff(&got);
        for x in [-2.0, 0.5, 4.0] {
            assert_close(at(&got, &[("x", x)]), 3.0 * x * x + 3.0);
        }
    }

    // -- the rule set itself ------------------------------------------------

    #[test]
    fn every_rule_has_a_unique_name() {
        let mut all = safe();
        all.extend(fast_math());
        assert!(!all.is_empty());

        let mut names: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two rules share a name: {:?}", names);
    }

    #[test]
    fn fast_math_adds_nothing() {
        assert!(fast_math().is_empty());
    }
}
