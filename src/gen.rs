//! Random expression generation.
//!
//! Used by the differential tests and by `saturn fuzz`, which is how the
//! claim that the safe rule tier preserves IEEE-754 results gets tested
//! rather than merely asserted.

use crate::lang::{Id, Op, RecExpr};
use crate::rng::Rng;

/// Which operators a generated expression may use.
#[derive(Clone, Debug)]
pub struct Grammar {
    pub vars: Vec<String>,
    /// Literals to draw from, in addition to freshly sampled floats.
    pub constants: Vec<f64>,
    pub binary: Vec<Op>,
    pub unary: Vec<Op>,
    /// Include comparisons, boolean connectives, and `if`.
    pub logic: bool,
    /// Include `d(x, e)` nodes.
    pub derivatives: bool,
    /// Chance at each step of reusing an already-built subterm instead of
    /// building a fresh one. Sharing is what makes a generated expression look
    /// like real code rather than a tree of unrelated parts.
    pub share: f64,
}

impl Default for Grammar {
    fn default() -> Grammar {
        Grammar {
            vars: vec!["x".into(), "y".into(), "z".into()],
            constants: vec![0.0, 1.0, 2.0, -1.0, 0.5, 3.0, 10.0],
            binary: vec![
                Op::Add,
                Op::Sub,
                Op::Mul,
                Op::Div,
                Op::Pow,
                Op::Min,
                Op::Max,
            ],
            unary: vec![
                Op::Neg,
                Op::Sqrt,
                Op::Ln,
                Op::Exp,
                Op::Sin,
                Op::Cos,
                Op::Abs,
            ],
            logic: false,
            derivatives: false,
            share: 0.25,
        }
    }
}

impl Grammar {
    /// Arithmetic only — no transcendentals, so results stay in a range where
    /// a disagreement is easy to attribute.
    pub fn arithmetic() -> Grammar {
        Grammar {
            binary: vec![Op::Add, Op::Sub, Op::Mul, Op::Div],
            unary: vec![Op::Neg, Op::Abs],
            ..Grammar::default()
        }
    }

    pub fn with_logic(mut self) -> Grammar {
        self.logic = true;
        self
    }

    pub fn with_derivatives(mut self) -> Grammar {
        self.derivatives = true;
        self
    }

    pub fn with_vars(mut self, vars: &[&str]) -> Grammar {
        self.vars = vars.iter().map(|s| s.to_string()).collect();
        self
    }
}

/// Build a random expression of at most `depth` levels.
///
/// The result is a DAG, not a tree: subterms already built are reused with
/// probability `grammar.share`, so the generator exercises the sharing that
/// hashconsing is supposed to preserve.
pub fn random_expr(rng: &mut Rng, grammar: &Grammar, depth: usize) -> RecExpr {
    let mut expr = RecExpr::new();
    let mut built: Vec<Id> = Vec::new();
    let root = build(rng, grammar, depth, &mut expr, &mut built);
    expr.compact(root)
}

fn build(rng: &mut Rng, g: &Grammar, depth: usize, expr: &mut RecExpr, built: &mut Vec<Id>) -> Id {
    if !built.is_empty() && rng.bool(g.share) {
        return *rng.pick(built);
    }
    if depth == 0 || rng.bool(0.2) {
        let id = if g.vars.is_empty() || rng.bool(0.35) {
            let c = if rng.bool(0.7) && !g.constants.is_empty() {
                *rng.pick(&g.constants)
            } else {
                rng.tame_float()
            };
            expr.constant(c)
        } else {
            expr.var(rng.pick(&g.vars).as_str())
        };
        built.push(id);
        return id;
    }

    let choice = rng.below(100);
    let id = if g.derivatives && choice < 8 {
        let v = if g.vars.is_empty() {
            "x".to_string()
        } else {
            rng.pick(&g.vars).clone()
        };
        let var = expr.var(v.as_str());
        let body = build(rng, g, depth - 1, expr, built);
        expr.op(Op::Diff, vec![var, body])
    } else if g.logic && choice < 25 {
        match rng.below(3) {
            0 => {
                let c = build(rng, g, depth - 1, expr, built);
                let a = build(rng, g, depth - 1, expr, built);
                let b = build(rng, g, depth - 1, expr, built);
                expr.op(Op::If, vec![c, a, b])
            }
            1 => {
                let op = *rng.pick(&[Op::Lt, Op::Le, Op::Gt, Op::Ge, Op::Eq, Op::Ne]);
                let a = build(rng, g, depth - 1, expr, built);
                let b = build(rng, g, depth - 1, expr, built);
                expr.op(op, vec![a, b])
            }
            _ => {
                let op = *rng.pick(&[Op::And, Op::Or]);
                let a = build(rng, g, depth - 1, expr, built);
                let b = build(rng, g, depth - 1, expr, built);
                expr.op(op, vec![a, b])
            }
        }
    } else if !g.unary.is_empty() && choice < 45 {
        let op = *rng.pick(&g.unary);
        let a = build(rng, g, depth - 1, expr, built);
        expr.op(op, vec![a])
    } else {
        let op = *rng.pick(&g.binary);
        let a = build(rng, g, depth - 1, expr, built);
        let b = build(rng, g, depth - 1, expr, built);
        expr.op(op, vec![a, b])
    };
    built.push(id);
    id
}

/// An endless stream of distinct random expressions from one seed.
pub struct ExprStream {
    rng: Rng,
    grammar: Grammar,
    depth: usize,
}

impl ExprStream {
    pub fn new(seed: u64, grammar: Grammar, depth: usize) -> ExprStream {
        ExprStream {
            rng: Rng::seed(seed),
            grammar,
            depth,
        }
    }
}

impl Iterator for ExprStream {
    type Item = RecExpr;
    fn next(&mut self) -> Option<RecExpr> {
        Some(random_expr(&mut self.rng, &self.grammar, self.depth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_reproducible() {
        let g = Grammar::default();
        let a: Vec<String> = ExprStream::new(7, g.clone(), 4)
            .take(20)
            .map(|e| e.to_sexp())
            .collect();
        let b: Vec<String> = ExprStream::new(7, g, 4)
            .take(20)
            .map(|e| e.to_sexp())
            .collect();
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_differ() {
        let g = Grammar::default();
        let a: Vec<String> = ExprStream::new(1, g.clone(), 4)
            .take(20)
            .map(|e| e.to_sexp())
            .collect();
        let b: Vec<String> = ExprStream::new(2, g, 4)
            .take(20)
            .map(|e| e.to_sexp())
            .collect();
        assert_ne!(a, b);
    }

    #[test]
    fn generated_expressions_are_well_formed() {
        let mut rng = Rng::seed(99);
        let g = Grammar::default().with_logic().with_derivatives();
        for _ in 0..500 {
            let e = random_expr(&mut rng, &g, 5);
            assert!(!e.is_empty());
            // Children always precede their parent, which is what lets a
            // single forward pass evaluate the whole DAG.
            for (i, n) in e.nodes().iter().enumerate() {
                for c in &n.children {
                    assert!(c.index() < i);
                }
                assert_eq!(n.op.arity(), n.children.len());
            }
            assert_eq!(e.root().index(), e.len() - 1);
        }
    }

    #[test]
    fn sharing_actually_happens() {
        let mut rng = Rng::seed(5);
        let g = Grammar {
            share: 0.5,
            ..Grammar::default()
        };
        let shared = (0..200)
            .map(|_| random_expr(&mut rng, &g, 6))
            .filter(|e| (e.dag_size() as u128) < e.tree_size())
            .count();
        assert!(shared > 100, "only {} of 200 expressions shared", shared);
    }

    #[test]
    fn derivative_nodes_name_a_variable() {
        let mut rng = Rng::seed(11);
        let g = Grammar::default().with_derivatives();
        for _ in 0..300 {
            let e = random_expr(&mut rng, &g, 5);
            for id in e.reachable(e.root()) {
                let n = e.node(id);
                if n.op == Op::Diff {
                    assert!(e.node(n.children[0]).as_var().is_some());
                }
            }
        }
    }
}
