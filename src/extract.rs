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
use std::collections::{HashMap, HashSet};

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
        1.0 + node.children().iter().map(|&c| child_cost(c)).sum::<f64>()
    }
}

/// The shallowest expression tree — minimizes the critical path rather than
/// the total work, which is what matters on a wide machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct AstDepth;

impl CostFunction for AstDepth {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64 {
        1.0 + node
            .children()
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
        own + node.children().iter().map(|&c| child_cost(c)).sum::<f64>()
    }
}

/// A cost function from a closure.
pub struct FnCost<F>(pub F);

impl<F: Fn(&ENode, &dyn Fn(Id) -> f64) -> f64> CostFunction for FnCost<F> {
    fn cost(&self, node: &ENode, child_cost: &dyn Fn(Id) -> f64) -> f64 {
        (self.0)(node, child_cost)
    }
}

/// A chosen e-node for each e-class, with the cost of the term it roots.
type Selection = HashMap<Id, (f64, ENode)>;

/// Solve `cost[class] = min over nodes of cost(node)` by iterating to a
/// fixpoint.
///
/// Costs only ever decrease and are bounded below, so this terminates. A class
/// whose every node sits in a cycle with no grounded base case simply never
/// gets a cost, which is the right answer: no finite term in it exists.
///
/// Classes in `free` cost nothing to *use*. That is how [`DagExtractor`]
/// expresses "this subterm is already being emitted, so a second reference to
/// it is not a second computation".
fn solve<A: Analysis, C: CostFunction>(
    egraph: &EGraph<A>,
    cost_fn: &C,
    free: &HashSet<Id>,
) -> Selection {
    let mut best: Selection = HashMap::new();
    loop {
        let mut changed = false;
        for class in egraph.classes() {
            let mut current: Option<(f64, ENode)> = None;
            for node in &class.nodes {
                let mut known = true;
                for &c in node.children() {
                    let c = egraph.find(c);
                    if !free.contains(&c) && !best.contains_key(&c) {
                        known = false;
                        break;
                    }
                }
                if !known {
                    continue;
                }
                let lookup = |id: Id| {
                    let id = egraph.find(id);
                    if free.contains(&id) {
                        0.0
                    } else {
                        best[&id].0
                    }
                };
                let c = cost_fn.cost(node, &lookup);
                if current.as_ref().map(|(bc, _)| c < *bc).unwrap_or(true) {
                    current = Some((c, *node));
                }
            }
            let Some((c, node)) = current else { continue };
            match best.get(&class.id) {
                Some((old, _)) if *old <= c => {}
                _ => {
                    best.insert(class.id, (c, node));
                    changed = true;
                }
            }
        }
        if !changed {
            return best;
        }
    }
}

/// Build the term a selection roots at `root`, or `None` if the selection is
/// cyclic there.
///
/// The optimal tree-cost selection is always acyclic, because every non-leaf
/// operator costs something and a cycle would need a node that costs nothing.
/// A *discounted* selection has no such guarantee, so this reports the problem
/// instead of looping.
fn build_from<A: Analysis>(egraph: &EGraph<A>, selection: &Selection, root: Id) -> Option<RecExpr> {
    fn go<A: Analysis>(
        egraph: &EGraph<A>,
        selection: &Selection,
        class: Id,
        expr: &mut RecExpr,
        memo: &mut HashMap<Id, Id>,
        on_stack: &mut Vec<Id>,
    ) -> Option<Id> {
        let class = egraph.find(class);
        if let Some(&id) = memo.get(&class) {
            return Some(id);
        }
        if on_stack.contains(&class) {
            return None;
        }
        on_stack.push(class);
        let node = &selection.get(&class)?.1;
        let mut children = Vec::with_capacity(node.children().len());
        for &c in node.children() {
            children.push(go(egraph, selection, c, expr, memo, on_stack)?);
        }
        on_stack.pop();
        let id = expr.op(node.op, children);
        memo.insert(class, id);
        Some(id)
    }

    let root = egraph.find(root);
    let mut expr = RecExpr::new();
    let mut memo = HashMap::new();
    let id = go(
        egraph,
        selection,
        root,
        &mut expr,
        &mut memo,
        &mut Vec::new(),
    )?;
    Some(expr.compact(id))
}

/// The e-classes a selection actually materializes, reachable from `root`.
fn materialized<A: Analysis>(egraph: &EGraph<A>, selection: &Selection, root: Id) -> HashSet<Id> {
    let mut seen = HashSet::new();
    let mut stack = vec![egraph.find(root)];
    while let Some(c) = stack.pop() {
        if !seen.insert(c) {
            continue;
        }
        if let Some((_, node)) = selection.get(&c) {
            for &child in node.children() {
                stack.push(egraph.find(child));
            }
        }
    }
    seen
}

/// Extracts the cheapest term from each e-class, minimizing cost over the
/// expression **tree**.
pub struct Extractor<'a, A: Analysis, C: CostFunction> {
    egraph: &'a EGraph<A>,
    cost_fn: C,
    best: Selection,
}

impl<'a, A: Analysis, C: CostFunction> Extractor<'a, A, C> {
    pub fn new(egraph: &'a EGraph<A>, cost_fn: C) -> Extractor<'a, A, C> {
        let best = solve(egraph, &cost_fn, &HashSet::new());
        Extractor {
            egraph,
            cost_fn,
            best,
        }
    }

    /// The cost function this extractor was built with.
    pub fn cost_function(&self) -> &C {
        &self.cost_fn
    }

    /// The tree cost of the cheapest term in `class`, if it has one.
    pub fn cost_of(&self, class: Id) -> Option<f64> {
        self.best.get(&self.egraph.find(class)).map(|(c, _)| *c)
    }

    /// The cheapest node in `class`.
    pub fn best_node(&self, class: Id) -> Option<&ENode> {
        self.best.get(&self.egraph.find(class)).map(|(_, n)| n)
    }

    /// The cheapest term in `class`, as a maximally-shared expression DAG.
    ///
    /// Panics if `class` contains no finite term, which can only happen when
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
        let expr = build_from(self.egraph, &self.best, root)?;
        Some((cost, expr))
    }
}

/// Extraction that accounts for sharing.
///
/// Tree cost double-counts a subterm used twice, so the tree-optimal choice
/// can be worse than another once common subexpressions are emitted only once.
/// Choosing the true DAG-optimal term is NP-hard. This does the standard
/// thing: extract by tree cost, then repeatedly re-solve with every class the
/// current answer already materializes priced at zero, keeping whichever round
/// produced the cheapest DAG.
///
/// It is a heuristic. It never returns something worse than the tree-optimal
/// answer, because that answer is the first candidate, but it is not
/// guaranteed optimal.
pub struct DagExtractor<'a, A: Analysis, C: CostFunction> {
    egraph: &'a EGraph<A>,
    cost_fn: C,
    rounds: usize,
}

impl<'a, A: Analysis, C: CostFunction> DagExtractor<'a, A, C> {
    pub fn new(egraph: &'a EGraph<A>, cost_fn: C) -> DagExtractor<'a, A, C> {
        DagExtractor {
            egraph,
            cost_fn,
            rounds: 6,
        }
    }

    /// How many refinement rounds to run. More rounds cost time and rarely
    /// help after the first few, which is why the default is small.
    pub fn with_rounds(mut self, n: usize) -> Self {
        self.rounds = n;
        self
    }

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
        let mut selection = solve(self.egraph, &self.cost_fn, &HashSet::new());
        let mut best = build_from(self.egraph, &selection, root)?;
        let mut best_cost = dag_cost(&best, &self.cost_fn);

        for _ in 0..self.rounds {
            let mut free = materialized(self.egraph, &selection, root);
            // The root is not free: something has to pay for it.
            free.remove(&root);
            let candidate = solve(self.egraph, &self.cost_fn, &free);
            let Some(expr) = build_from(self.egraph, &candidate, root) else {
                break;
            };
            let cost = dag_cost(&expr, &self.cost_fn);
            if cost < best_cost {
                best = expr;
                best_cost = cost;
                selection = candidate;
            } else {
                break;
            }
        }
        Some((best_cost, best))
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
