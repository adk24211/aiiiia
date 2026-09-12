//! E-class analyses: facts that hold for every term in an e-class.
//!
//! An analysis is a meet-semilattice. [`Analysis::make`] computes a fact for a
//! single e-node from its children's facts; [`Analysis::merge`] combines two
//! facts about the *same* value; [`Analysis::modify`] may act on the e-graph
//! when a class's fact changes — constant folding uses it to union a class
//! with the literal it was proven equal to.

use crate::egraph::EGraph;
use crate::interval::Interval;
use crate::lang::{ENode, Id, Op};
use crate::sym::F;

pub trait Analysis: Sized {
    type Data: Clone + Eq + std::fmt::Debug;

    /// The fact implied by a single e-node, given its children's facts.
    fn make(egraph: &EGraph<Self>, node: &ENode) -> Self::Data;

    /// Combine two facts about the same value. Must be commutative,
    /// associative, and idempotent.
    fn merge(&mut self, a: Self::Data, b: Self::Data) -> Self::Data;

    /// Called after a class's fact is established or changes. May add nodes
    /// and unions; the e-graph reaches a fixpoint before returning.
    fn modify(_egraph: &mut EGraph<Self>, _id: Id) {}
}

/// The trivial analysis: no facts, no folding.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoAnalysis;

impl Analysis for NoAnalysis {
    type Data = ();
    fn make(_: &EGraph<Self>, _: &ENode) {}
    fn merge(&mut self, _: (), _: ()) {}
}

/// What [`MathAnalysis`] knows about the value an e-class denotes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MathData {
    /// The exact value, when it is provably a single finite double.
    ///
    /// Kept separately from `range` because constant folding must be exact:
    /// `range` rounds outward and so can only ever bracket a value.
    pub constant: Option<F>,
    /// Sound bounds, used by the side conditions on rewrite rules.
    pub range: Interval,
}

impl MathData {
    pub const TOP: MathData = MathData {
        constant: None,
        range: Interval::TOP,
    };

    pub fn of(x: f64) -> MathData {
        MathData {
            constant: if x.is_finite() { Some(F::new(x)) } else { None },
            range: Interval::point(x),
        }
    }

    pub fn value(&self) -> Option<f64> {
        self.constant.map(|c| c.get())
    }
}

impl Default for MathData {
    fn default() -> MathData {
        MathData::TOP
    }
}

/// Constant folding plus interval analysis.
///
/// `fold_transcendental` controls whether `sin`, `ln`, `exp`, `pow` and
/// friends are evaluated at compile time. They are correctly rounded on most
/// platforms but not required to be, so a build that must reproduce the
/// runtime's results bit for bit can turn them off.
#[derive(Clone, Copy, Debug)]
pub struct MathAnalysis {
    pub fold_transcendental: bool,
}

impl Default for MathAnalysis {
    fn default() -> MathAnalysis {
        MathAnalysis {
            fold_transcendental: true,
        }
    }
}

fn is_transcendental(op: Op) -> bool {
    matches!(
        op,
        Op::Ln | Op::Exp | Op::Sin | Op::Cos | Op::Tan | Op::Pow | Op::Atan2
    )
}

impl Analysis for MathAnalysis {
    type Data = MathData;

    fn make(egraph: &EGraph<Self>, node: &ENode) -> MathData {
        let child = |i: usize| egraph[node.children[i]].data;

        // -- constant folding ------------------------------------------------
        let constant = (|| -> Option<F> {
            if let Op::Const(c) = node.op {
                return Some(c);
            }
            if !node.op.is_foldable() {
                return None;
            }
            if is_transcendental(node.op) && !egraph.analysis.fold_transcendental {
                return None;
            }
            let mut args = Vec::with_capacity(node.children.len());
            for i in 0..node.children.len() {
                args.push(child(i).value()?);
            }
            let v = node.op.eval(&args)?;
            // Only fold to finite results: turning an expression into `inf` or
            // `NaN` would discard the very information a later rule needs.
            v.is_finite().then(|| F::new(v))
        })();

        // -- interval --------------------------------------------------------
        let range = match node.op {
            Op::Const(c) => Interval::point(c.get()),
            Op::Var(_) => Interval::TOP,
            Op::Add => child(0).range.add(child(1).range),
            Op::Sub => child(0).range.sub(child(1).range),
            Op::Mul => child(0).range.mul(child(1).range),
            Op::Div => child(0).range.div(child(1).range),
            Op::Pow => child(0).range.pow(child(1).range),
            Op::Min => child(0).range.min(child(1).range),
            Op::Max => child(0).range.max(child(1).range),
            Op::Atan2 => Interval {
                lo: -std::f64::consts::PI,
                hi: std::f64::consts::PI,
                nan: child(0).range.nan || child(1).range.nan,
            },
            Op::Neg => child(0).range.neg(),
            Op::Sqrt => child(0).range.sqrt(),
            Op::Ln => child(0).range.ln(),
            Op::Exp => child(0).range.exp(),
            Op::Sin | Op::Cos => child(0).range.bounded_trig(),
            Op::Tan => Interval::TOP,
            Op::Abs => child(0).range.abs(),
            Op::Sign => child(0).range.sign(),
            Op::Floor => child(0).range.floor(),
            Op::Ceil => child(0).range.ceil(),
            Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::And | Op::Or | Op::Not => {
                Interval::BOOL
            }
            Op::If => child(1).range.join(child(2).range),
            Op::Diff => Interval::TOP,
        };

        let range = match constant {
            Some(c) => range.meet(Interval::point(c.get())),
            None => range,
        };
        MathData { constant, range }
    }

    fn merge(&mut self, a: MathData, b: MathData) -> MathData {
        let constant = match (a.constant, b.constant) {
            (Some(x), Some(y)) => {
                debug_assert_eq!(
                    x, y,
                    "two different constants proven equal — a rewrite rule is unsound"
                );
                Some(x)
            }
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        };
        MathData {
            constant,
            range: a.range.meet(b.range),
        }
    }

    fn modify(egraph: &mut EGraph<Self>, id: Id) {
        let Some(c) = egraph[id].data.constant else {
            return;
        };
        // Nothing to do if the class already contains the literal.
        if egraph[id]
            .nodes
            .iter()
            .any(|n| n.as_const() == Some(c.get()))
        {
            return;
        }
        let lit = egraph.add(ENode::leaf(Op::Const(c)));
        egraph.union_because(id, lit, crate::explain::Justification::Fold);
    }
}
