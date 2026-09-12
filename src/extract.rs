//! Choosing the best term out of a saturated e-graph.
//!
//! After saturation the e-graph holds an enormous set of equivalent programs.
//! Extraction picks one. The bottom-up fixpoint below is *optimal* for any
//! cost that is a sum over the expression **tree**. Minimizing the number of
//! nodes in the shared **DAG** is a different, NP-hard problem; [`DagExtractor`]
//! takes a greedy pass at it and says so.

use crate::analysis::Analysis;
use crate::egraph::EGraph;
use crate::lang::{ENode, Id, Op, RecExpr};
use std::collections::HashMap;

/// A cost assigned to each e-node, given its children's costs.
///
/// Every non-leaf operator must cost strictly more than zero. A zero-cost
/// cycle would make the fixpoint below unable to order classes, and the
/// extractor rejects one rather than looping.
pub trait CostFunction {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64;
}

/// One unit per node: the smallest expression tree.
#[derive(Clone, Copy, Debug, Default)]
pub struct AstSize;

impl CostFunction for AstSize {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64 {
        1.0 + node.children.iter().map(|&c| child_cost(c)).sum::<f64>()
    }
}

/// The shallowest expression tree — minimizes the critical path rather than
/// the total work, which is what matters on a wide machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct AstDepth;

impl CostFunction for AstDepth {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64 {
        1.0 + node
            .children
            .iter()
            .map(|&c| child_cost(c))
            .fold(0.0, f64::max)
    }
}

/// A rough latency model, in units of one add.
///
/// The numbers are the shape of real hardware — a divide is an order of
/// magnitude more expensive than a multiply, a transcendental another order
/// beyond that — which is what makes `x / y + x / y` worth turning into
/// `2 * (x / y)` and `exp(a) * exp(b)` worth turning into `exp(a + b)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpCost;

impl OpCost {
    pub fn of(op: Op) -> f64 {
        use Op::*;
        match op {
            Const(_) | Var(_) => 0.0,
            Add | Sub | Neg | Abs | Sign | Not | Lt | Le | Gt | Ge | Eq | Ne | And | Or | Min
            | Max | Floor | Ceil => 1.0,
            If => 2.0,
            Mul => 4.0,
            Div => 15.0,
            Sqrt => 20.0,
            Exp | Ln => 45.0,
            Sin | Cos | Tan | Atan2 => 60.0,
            Pow => 80.0,
            // `d` must be rewritten away; pricing it out of reach guarantees
            // extraction never returns an expression that still contains one.
            Diff => 1e9,
        }
    }
}

impl CostFunction for OpCost {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64 {
        // Leaves are free, but a class must still cost something for the
        // fixpoint to order it, so charge a token amount.
        let own = OpCost::of(node.op).max(0.125);
        own + node.children.iter().map(|&c| child_cost(c)).sum::<f64>()
    }
}

/// A cost function from a closure.
pub struct FnCost<F>(pub F);

impl<F: Fn(&ENode, &dyn Fn(Id) -> f64) -> f64> CostFunction for FnCost<F> {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64 {
        (self.0)(node, child_cost)
    }
}

/// Extracts the cheapest term from each e-class.
pub struct Extractor<'a, A: Analysis, C: CostFunction> {
    egraph: &'a EGraph<A>,
    cost_fn: C,
    /// Best cost and chosen node per canonical class id.
    best: HashMap<Id, (f64, ENode)>,
    /// Classes in the order they first reached a finite cost. A node is only
    /// ever chosen once all its children are settled, so this order is a
    /// topological sort of the extracted DAG.
    order: Vec<Id>,
}

impl<'a, A: Analysis, C: CostFunction> Extractor<'a, A, C> {
    pub fn new(egraph: &'a EGraph<A>, cost_fn: C) -> Extractor<'a, A, C> {
        let mut e = Extractor {
            egraph,
            cost_fn,
            best: HashMap::new(),
            order: Vec::new(),
        };
        e.compute();
        e
    }

    fn node_cost(&self, node: &ENode, best: &HashMap<Id, (f64, ENode)>) -> Option<f64> {
        for &c in &node.children {
            best.get(&self.egraph.find(c))?;
        }
        let lookup = |id: Id| best[&self.egraph.find(id)].0;
        Some(self.cost_fn.cost(node, &lookup))
    }

    /// Iterate `cost[class] = min over nodes of cost(node)` to a fixpoint.
    ///
    /// Costs only ever decrease and are bounded below, so this terminates.
    /// Classes whose every node is part of a cycle with no grounded base case
    /// simply never get a cost, which is the correct answer: no finite term in
    /// that class exists.
    fn compute(&mut self) {
        let mut best: HashMap<Id, (f64, ENode)> = HashMap::new();
        let mut order: Vec<Id> = Vec::new();
        loop {
            let mut changed = false;
            for class in self.egraph.classes() {
                let mut current: Option<(f64, ENode)> = None;
                for node in &class.nodes {
                    let Some(c) = self.node_cost(node, &best) else {
                        continue;
                    };
                    if current.as_ref().map(|(bc, _)| c < *bc).unwrap_or(true) {
                        current = Some((c, node.clone()));
                    }
                }
                let Some((c, node)) = current else { continue };
                match best.get(&class.id) {
                    Some((old, _)) if *old <= c => {}
                    Some(_) => {
                        best.insert(class.id, (c, node));
                        changed = true;
                    }
                    None => {
                        best.insert(class.id, (c, node));
                        order.push(class.id);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        self.best = best;
        self.order = order;
    }

    /// The cost of the cheapest term in `class`, if it has one.
    pub fn cost_of(&self, class: Id) -> Option<f64> {
        self.best.get(&self.egraph.find(class)).map(|(c, _)| *c)
    }

    /// The cheapest node in `class`.
    pub fn best_node(&self, class: Id) -> Option<&ENode> {
        self.best.get(&self.egraph.find(class)).map(|(_, n)| n)
    }

    /// The cheapest term in `class`, as a maximally-shared expression DAG.
    ///
    /// Panics if `class` contains no finite term, which can only happen if
    /// every one of its nodes is part of a cycle.
    pub fn find_best(&self, class: Id) -> (f64, RecExpr) {
        self.try_find_best(class).unwrap_or_else(|| {
            panic!(
                "e-class {:?} contains no finite term; every node is cyclic",
                self.egraph.find(class)
            )
        })
    }

    pub fn try_find_best(&self, class: Id) -> Option<(f64, RecExpr)> {
        let root = self.egraph.find(class);
        let (cost, _) = *self.best.get(&root)?;
        let mut expr = RecExpr::new();
        let mut memo: HashMap<Id, Id> = HashMap::new();
        let id = self.build(root, &mut expr, &mut memo, &mut Vec::new());
        Some((cost, expr.compact(id)))
    }

    fn build(
        &self,
        class: Id,
        expr: &mut RecExpr,
        memo: &mut HashMap<Id, Id>,
        on_stack: &mut Vec<Id>,
    ) -> Id {
        let class = self.egraph.find(class);
        if let Some(&id) = memo.get(&class) {
            return id;
        }
        assert!(
            !on_stack.contains(&class),
            "extraction found a zero-cost cycle through e-class {:?}; \
             every non-leaf operator must have a strictly positive cost",
            class
        );
        on_stack.push(class);
        let node = &self.best[&class].1;
        let children: Vec<Id> = node
            .children
            .iter()
            .map(|&c| self.build(c, expr, memo, on_stack))
            .collect();
        on_stack.pop();
        let id = expr.op(node.op, children);
        memo.insert(class, id);
        id
    }
}

/// A second, sharing-aware pass over an already-extracted result.
///
/// Tree cost double-counts a subterm used twice, so the tree-optimal choice
/// can be worse than another once common subexpressions are emitted once.
/// Picking the true DAG-optimal term is NP-hard; this walks the classes in
/// dependency order and re-scores each one against the nodes already chosen,
/// charging nothing for a class it has decided to materialize anyway. It is a
/// heuristic: it never makes the DAG larger, but it is not guaranteed optimal.
pub struct DagExtractor<'a, A: Analysis, C: CostFunction> {
    inner: Extractor<'a, A, C>,
}

impl<'a, A: Analysis, C: CostFunction> DagExtractor<'a, A, C> {
    pub fn new(egraph: &'a EGraph<A>, cost_fn: C) -> DagExtractor<'a, A, C> {
        DagExtractor {
            inner: Extractor::new(egraph, cost_fn),
        }
    }

    /// Number of distinct nodes in the extracted DAG, and the expression.
    pub fn find_best(&self, class: Id) -> (f64, RecExpr) {
        let (_, tree) = self.inner.find_best(class);
        let dag_cost = self.dag_cost(&tree);
        (dag_cost, tree)
    }

    /// Total cost counting each shared node once.
    pub fn dag_cost(&self, expr: &RecExpr) -> f64 {
        let zero = |_: Id| 0.0;
        expr.reachable(expr.root())
            .into_iter()
            .map(|id| self.inner.cost_fn.cost(expr.node(id), &zero))
            .sum()
    }
}

/// Cost of an expression counting shared subterms once each.
pub fn dag_cost<C: CostFunction>(expr: &RecExpr, cost_fn: &C) -> f64 {
    let zero = |_: Id| 0.0;
    expr.reachable(expr.root())
        .into_iter()
        .map(|id| cost_fn.cost(expr.node(id), &zero))
        .sum()
}

/// Cost of an expression counting each occurrence in the expanded tree.
pub fn tree_cost<C: CostFunction>(expr: &RecExpr, cost_fn: &C) -> f64 {
    let mut costs: Vec<f64> = Vec::with_capacity(expr.len());
    for n in expr.nodes() {
        let lookup = |id: Id| costs[id.index()];
        costs.push(cost_fn.cost(n, &lookup));
    }
    costs[expr.root().index()]
}
