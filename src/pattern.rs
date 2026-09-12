//! Patterns and e-matching.
//!
//! A pattern is an expression tree whose leaves may be *pattern variables*
//! (`?x`). E-matching finds every way to instantiate those variables so that
//! the pattern denotes a term in a given e-class. Because an e-class stands
//! for many terms, a single pattern can match in many ways, and the matcher
//! returns all of them.

use crate::analysis::Analysis;
use crate::egraph::EGraph;
use crate::lang::{ENode, Id, Op, RecExpr};
use crate::parser;
use crate::sym::Sym;
use std::fmt;

/// A variable binding produced by matching.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Subst {
    bindings: Vec<(Sym, Id)>,
}

impl Subst {
    pub fn new() -> Subst {
        Subst::default()
    }

    #[inline]
    pub fn get(&self, v: Sym) -> Option<Id> {
        self.bindings.iter().find(|(s, _)| *s == v).map(|(_, i)| *i)
    }

    /// Bind `v`, replacing any previous binding.
    pub fn insert(&mut self, v: Sym, id: Id) {
        match self.bindings.iter_mut().find(|(s, _)| *s == v) {
            Some(slot) => slot.1 = id,
            None => {
                self.bindings.push((v, id));
                // Keeping bindings sorted makes `Subst` comparable, which is
                // what lets the matcher deduplicate its results.
                self.bindings.sort();
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Sym, Id)> + '_ {
        self.bindings.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Rewrite every bound id to its canonical representative.
    pub fn canonicalize<A: Analysis>(&mut self, egraph: &EGraph<A>) {
        for (_, id) in self.bindings.iter_mut() {
            *id = egraph.find(*id);
        }
        self.bindings.sort();
    }
}

impl fmt::Display for Subst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (i, (s, id)) in self.bindings.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{} -> {}", s, id)?;
        }
        f.write_str("}")
    }
}

/// One node of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatNode {
    /// `?x`
    Var(Sym),
    /// An operator applied to earlier pattern nodes.
    Op(Op, Vec<usize>),
}

/// A flat, topologically-sorted pattern; the root is the last node.
#[derive(Clone, PartialEq, Eq)]
pub struct Pattern {
    nodes: Vec<PatNode>,
    vars: Vec<Sym>,
}

/// Every substitution found for one e-class.
#[derive(Clone, Debug)]
pub struct SearchMatches {
    pub eclass: Id,
    pub substs: Vec<Subst>,
}

impl SearchMatches {
    pub fn len(&self) -> usize {
        self.substs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.substs.is_empty()
    }
}

/// A hard cap on the substitutions a single e-class may produce, so that a
/// pathological pattern cannot hang the matcher.
const MAX_SUBSTS_PER_CLASS: usize = 2_000;

impl Pattern {
    /// Build a pattern from a parsed expression. Variables whose names begin
    /// with `?` become pattern variables; every other variable is a literal
    /// symbol that must match exactly.
    pub fn from_expr(expr: &RecExpr) -> Pattern {
        let mut nodes = Vec::with_capacity(expr.len());
        let mut vars = Vec::new();
        let ids = expr.reachable(expr.root());
        let mut map = std::collections::HashMap::new();
        for id in ids {
            let n = expr.node(id);
            let pat = match n.as_var() {
                Some(s) if s.as_str().starts_with('?') => {
                    if !vars.contains(&s) {
                        vars.push(s);
                    }
                    PatNode::Var(s)
                }
                _ => PatNode::Op(n.op, n.children.iter().map(|c| map[c]).collect()),
            };
            map.insert(id, nodes.len());
            nodes.push(pat);
        }
        Pattern { nodes, vars }
    }

    /// Parse a pattern from source, e.g. `"?a * (?b + ?c)"`.
    pub fn parse(src: &str) -> Result<Pattern, crate::lexer::ParseError> {
        Ok(Pattern::from_expr(&parser::parse(src)?))
    }

    pub fn nodes(&self) -> &[PatNode] {
        &self.nodes
    }

    pub fn root(&self) -> usize {
        self.nodes.len() - 1
    }

    /// The pattern variables, in first-occurrence order.
    pub fn vars(&self) -> &[Sym] {
        &self.vars
    }

    /// The operator at the root, if the root is not a bare variable.
    pub fn root_op(&self) -> Option<Op> {
        match &self.nodes[self.root()] {
            PatNode::Op(op, _) => Some(*op),
            PatNode::Var(_) => None,
        }
    }

    /// True if this pattern is a single variable, which matches every e-class
    /// and is therefore never a useful left-hand side.
    pub fn is_trivial(&self) -> bool {
        self.nodes.len() == 1 && matches!(self.nodes[0], PatNode::Var(_))
    }

    /// Number of operator nodes — a rough measure of how selective a pattern is.
    pub fn size(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n, PatNode::Op(..)))
            .count()
    }

    // -- matching -----------------------------------------------------------

    /// Every substitution under which this pattern denotes a term in `class`.
    pub fn search_eclass<A: Analysis>(
        &self,
        egraph: &EGraph<A>,
        class: Id,
    ) -> Option<SearchMatches> {
        let mut out = Vec::new();
        self.match_node(egraph, self.root(), class, Subst::new(), &mut out);
        if out.is_empty() {
            return None;
        }
        out.sort();
        out.dedup();
        Some(SearchMatches {
            eclass: egraph.find(class),
            substs: out,
        })
    }

    /// Search every e-class. E-classes are visited in id order, so the result
    /// is deterministic.
    pub fn search<A: Analysis>(&self, egraph: &EGraph<A>) -> Vec<SearchMatches> {
        let root_op = self.root_op();
        egraph
            .classes()
            .filter(|c| match root_op {
                // Skip classes that cannot possibly contain the root operator.
                Some(op) => c.nodes.iter().any(|n| n.op == op),
                None => true,
            })
            .filter_map(|c| self.search_eclass(egraph, c.id))
            .collect()
    }

    fn match_node<A: Analysis>(
        &self,
        egraph: &EGraph<A>,
        pat: usize,
        class: Id,
        subst: Subst,
        out: &mut Vec<Subst>,
    ) {
        if out.len() >= MAX_SUBSTS_PER_CLASS {
            return;
        }
        match &self.nodes[pat] {
            PatNode::Var(v) => {
                let class = egraph.find(class);
                match subst.get(*v) {
                    // A repeated variable must bind to the same e-class both
                    // times; this is what makes `?x - ?x => 0` sound.
                    Some(prev) => {
                        if egraph.find(prev) == class {
                            out.push(subst);
                        }
                    }
                    None => {
                        let mut s = subst;
                        s.insert(*v, class);
                        out.push(s);
                    }
                }
            }
            PatNode::Op(op, pat_children) => {
                for node in &egraph[class].nodes {
                    if node.op != *op {
                        continue;
                    }
                    if pat_children.is_empty() {
                        out.push(subst.clone());
                        continue;
                    }
                    self.match_children(egraph, pat_children, &node.children, subst.clone(), out);
                    // Commutative nodes are stored with their children in a
                    // canonical order, so the matcher must try the other one.
                    if op.is_commutative()
                        && node.children.len() == 2
                        && node.children[0] != node.children[1]
                    {
                        let swapped = [node.children[1], node.children[0]];
                        self.match_children(egraph, pat_children, &swapped, subst.clone(), out);
                    }
                }
            }
        }
    }

    fn match_children<A: Analysis>(
        &self,
        egraph: &EGraph<A>,
        pats: &[usize],
        args: &[Id],
        subst: Subst,
        out: &mut Vec<Subst>,
    ) {
        debug_assert_eq!(pats.len(), args.len());
        let mut frontier = vec![subst];
        for (&p, &a) in pats.iter().zip(args) {
            let mut next = Vec::new();
            for s in frontier {
                self.match_node(egraph, p, a, s, &mut next);
            }
            if next.is_empty() {
                return;
            }
            frontier = next;
        }
        out.extend(frontier);
    }

    // -- instantiation ------------------------------------------------------

    /// Add this pattern to the graph with `subst` applied, returning the id of
    /// the root. Every variable in the pattern must be bound.
    pub fn instantiate<A: Analysis>(&self, egraph: &mut EGraph<A>, subst: &Subst) -> Id {
        let mut ids: Vec<Id> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let id = match node {
                PatNode::Var(v) => subst.get(*v).unwrap_or_else(|| {
                    panic!(
                        "pattern variable {} is unbound when instantiating {}",
                        v, self
                    )
                }),
                PatNode::Op(op, children) => {
                    let cs = children.iter().map(|&c| ids[c]).collect();
                    egraph.add(ENode::new(*op, cs))
                }
            };
            ids.push(id);
        }
        ids[self.root()]
    }

    /// Build a standalone expression from this pattern and a map from variable
    /// name to expression. Used by the rule pretty-printer and tests.
    pub fn to_string_pretty(&self) -> String {
        self.write(self.root())
    }

    fn write(&self, i: usize) -> String {
        match &self.nodes[i] {
            PatNode::Var(v) => v.to_string(),
            PatNode::Op(Op::Const(c), _) => c.to_string(),
            PatNode::Op(Op::Var(s), _) => s.to_string(),
            PatNode::Op(op, cs) if op.is_infix() => {
                format!(
                    "({} {} {})",
                    self.write(cs[0]),
                    op.name(),
                    self.write(cs[1])
                )
            }
            PatNode::Op(op, cs) if cs.is_empty() => op.name().to_string(),
            PatNode::Op(op, cs) => format!(
                "{}({})",
                op.name(),
                cs.iter()
                    .map(|&c| self.write(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_pretty())
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_pretty())
    }
}

impl std::str::FromStr for Pattern {
    type Err = crate::lexer::ParseError;
    fn from_str(s: &str) -> Result<Pattern, Self::Err> {
        Pattern::parse(s)
    }
}
