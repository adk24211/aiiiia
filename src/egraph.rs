//! The e-graph: a union-find over e-classes, each holding a set of e-nodes,
//! maintained congruent by deferred rebuilding.
//!
//! The data structure maintains two invariants between calls to
//! [`EGraph::rebuild`]:
//!
//! * **hashcons** — every e-node, canonicalized, maps to the canonical id of
//!   the e-class that contains it;
//! * **congruence** — if two e-nodes have the same operator and pairwise
//!   equivalent children, they are in the same e-class.
//!
//! Both are temporarily broken by [`EGraph::union`] and restored in bulk by
//! `rebuild`, which is dramatically cheaper than restoring them eagerly.

use crate::analysis::Analysis;
use crate::lang::{ENode, Id, Op, RecExpr};
use crate::unionfind::UnionFind;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Index;

/// A set of e-nodes known to be equivalent, plus the analysis fact that holds
/// for the value they all denote.
#[derive(Clone, Debug)]
pub struct EClass<D> {
    /// The canonical id of this class.
    pub id: Id,
    /// Equivalent e-nodes, kept sorted and deduplicated after `rebuild`.
    pub nodes: Vec<ENode>,
    /// The analysis fact for this class.
    pub data: D,
    /// E-nodes elsewhere in the graph that reference this class, paired with
    /// the class that contains them. Used to restore congruence.
    pub(crate) parents: Vec<(ENode, Id)>,
}

impl<D> EClass<D> {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &ENode> {
        self.nodes.iter()
    }
    /// The literal value in this class, if one of its nodes is a constant.
    pub fn leaf_constant(&self) -> Option<f64> {
        self.nodes.iter().find_map(|n| n.as_const())
    }
}

/// Counters describing the shape of the graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EGraphStats {
    pub classes: usize,
    pub nodes: usize,
}

/// The e-graph.
pub struct EGraph<A: Analysis> {
    /// User-supplied analysis state (most analyses are stateless).
    pub analysis: A,
    unionfind: UnionFind,
    /// Canonical e-node -> id of the class containing it. Values may be stale
    /// and must be passed through `find`.
    memo: HashMap<ENode, Id>,
    /// Indexed by raw id; `Some` exactly at canonical ids.
    classes: Vec<Option<EClass<A::Data>>>,
    /// Classes whose parents may have lost congruence.
    pending: Vec<Id>,
    /// Classes whose analysis fact changed and must be re-propagated.
    analysis_pending: Vec<Id>,
    clean: bool,
}

impl<A: Analysis + Default> Default for EGraph<A> {
    fn default() -> Self {
        EGraph::new(A::default())
    }
}

impl<A: Analysis> EGraph<A> {
    pub fn new(analysis: A) -> EGraph<A> {
        EGraph {
            analysis,
            unionfind: UnionFind::new(),
            memo: HashMap::new(),
            classes: Vec::new(),
            pending: Vec::new(),
            analysis_pending: Vec::new(),
            clean: true,
        }
    }

    // -- queries ------------------------------------------------------------

    /// Canonical representative of `id`.
    #[inline]
    pub fn find(&self, id: Id) -> Id {
        self.unionfind.find_immutable(id)
    }

    /// Canonical representative of `id`, compressing paths as a side effect.
    #[inline]
    pub fn find_mut(&mut self, id: Id) -> Id {
        self.unionfind.find(id)
    }

    /// Number of e-classes.
    pub fn number_of_classes(&self) -> usize {
        self.classes.iter().filter(|c| c.is_some()).count()
    }

    /// Total number of e-nodes across all classes.
    pub fn total_nodes(&self) -> usize {
        self.classes
            .iter()
            .filter_map(|c| c.as_ref())
            .map(|c| c.nodes.len())
            .sum()
    }

    pub fn stats(&self) -> EGraphStats {
        EGraphStats {
            classes: self.number_of_classes(),
            nodes: self.total_nodes(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// True when no `union` has happened since the last `rebuild`.
    pub fn is_clean(&self) -> bool {
        self.clean
    }

    /// All e-classes, in ascending id order. Deterministic.
    pub fn classes(&self) -> impl Iterator<Item = &EClass<A::Data>> {
        self.classes.iter().filter_map(|c| c.as_ref())
    }

    /// All canonical class ids, ascending.
    pub fn class_ids(&self) -> Vec<Id> {
        self.classes().map(|c| c.id).collect()
    }

    /// The class containing `id`.
    pub fn class(&self, id: Id) -> &EClass<A::Data> {
        let id = self.find(id);
        self.classes[id.index()]
            .as_ref()
            .expect("canonical id has no class")
    }

    fn class_mut(&mut self, id: Id) -> &mut EClass<A::Data> {
        let id = self.find_mut(id);
        self.classes[id.index()]
            .as_mut()
            .expect("canonical id has no class")
    }

    /// Are `a` and `b` in the same e-class?
    pub fn equivalent(&self, a: Id, b: Id) -> bool {
        self.find(a) == self.find(b)
    }

    /// Rewrite `node`'s children to canonical ids and normalize commutative
    /// argument order. Every node stored in `memo` or in a class is canonical.
    pub fn canonicalize(&self, node: &ENode) -> ENode {
        let mut n = node.map_children(|c| self.find(c));
        n.normalize();
        n
    }

    /// The class containing `node`, if the graph already has it.
    pub fn lookup(&self, node: &ENode) -> Option<Id> {
        let n = self.canonicalize(node);
        self.memo.get(&n).map(|&id| self.find(id))
    }

    /// Look up an operator applied to children without allocating a node twice.
    pub fn lookup_op(&self, op: Op, children: Vec<Id>) -> Option<Id> {
        self.lookup(&ENode::new(op, children))
    }

    // -- construction -------------------------------------------------------

    /// Add `node` to the graph, returning the id of its class. If a congruent
    /// node is already present, no new class is created.
    pub fn add(&mut self, node: ENode) -> Id {
        let node = self.canonicalize(&node);
        if let Some(&existing) = self.memo.get(&node) {
            return self.find(existing);
        }

        let data = A::make(self, &node);

        let id = self.unionfind.make_set();
        debug_assert_eq!(id.index(), self.classes.len());
        self.classes.push(Some(EClass {
            id,
            nodes: vec![node.clone()],
            data,
            parents: Vec::new(),
        }));

        // Record this node as a parent of each of its children's classes, so
        // that unioning a child can find the nodes that must stay congruent.
        for &child in &node.children {
            let c = self.find(child);
            self.classes[c.index()]
                .as_mut()
                .expect("child class")
                .parents
                .push((node.clone(), id));
        }

        self.memo.insert(node, id);
        A::modify(self, id);
        self.find(id)
    }

    /// Add `op` applied to `children`.
    pub fn add_op(&mut self, op: Op, children: Vec<Id>) -> Id {
        self.add(ENode::new(op, children))
    }

    pub fn add_constant(&mut self, x: f64) -> Id {
        self.add(ENode::constant(x))
    }

    /// Add every node of `expr`, returning the class of its root.
    pub fn add_expr(&mut self, expr: &RecExpr) -> Id {
        let mut map: HashMap<Id, Id> = HashMap::new();
        let mut last = None;
        for (i, n) in expr.nodes().iter().enumerate() {
            let id = self.add(ENode::new(
                n.op,
                n.children.iter().map(|c| map[c]).collect(),
            ));
            map.insert(Id::new(i), id);
            last = Some(id);
        }
        last.expect("cannot add an empty expression")
    }

    /// Add `expr` rooted at an arbitrary node rather than its last.
    pub fn add_expr_from(&mut self, expr: &RecExpr, root: Id) -> Id {
        let compacted = expr.compact(root);
        self.add_expr(&compacted)
    }

    /// Declare `a` and `b` equal. Returns `true` if they were not already.
    ///
    /// This leaves the graph *dirty*: congruence is restored by the next
    /// [`EGraph::rebuild`].
    pub fn union(&mut self, a: Id, b: Id) -> bool {
        let (a, b) = (self.find_mut(a), self.find_mut(b));
        if a == b {
            return false;
        }
        let (root, absorbed) = self.unionfind.union(a, b);
        let other = self.classes[absorbed.index()]
            .take()
            .expect("absorbed class must exist");

        let root_class = self.classes[root.index()]
            .as_mut()
            .expect("root class must exist");
        let old_data = root_class.data.clone();
        root_class.nodes.extend(other.nodes);
        root_class.parents.extend(other.parents);
        let merged = self.analysis.merge(old_data.clone(), other.data);
        let changed = merged != old_data;
        self.classes[root.index()].as_mut().unwrap().data = merged;

        self.pending.push(root);
        if changed {
            self.analysis_pending.push(root);
        }
        self.clean = false;
        true
    }

    /// Union the classes of two expressions, returning whether anything changed.
    pub fn union_exprs(&mut self, a: &RecExpr, b: &RecExpr) -> bool {
        let ia = self.add_expr(a);
        let ib = self.add_expr(b);
        self.union(ia, ib)
    }

    // -- rebuilding ---------------------------------------------------------

    /// Restore the hashcons and congruence invariants, then run the analysis
    /// to a fixpoint. Returns the number of e-class merges performed.
    pub fn rebuild(&mut self) -> usize {
        let mut unions = 0;
        loop {
            unions += self.restore_congruence();
            for id in self.propagate_analysis() {
                if self.classes[self.find(id).index()].is_some() {
                    A::modify(self, id);
                }
            }
            if self.pending.is_empty() && self.analysis_pending.is_empty() {
                break;
            }
        }
        self.reindex();
        self.clean = true;
        debug_assert!(self.pending.is_empty());
        unions
    }

    fn restore_congruence(&mut self) -> usize {
        let mut unions = 0;
        while !self.pending.is_empty() {
            let mut todo: Vec<Id> = std::mem::take(&mut self.pending)
                .into_iter()
                .map(|id| self.find_mut(id))
                .collect();
            todo.sort_unstable();
            todo.dedup();
            for id in todo {
                unions += self.repair(id);
            }
        }
        unions
    }

    /// Restore congruence for every node that references `id`.
    fn repair(&mut self, id: Id) -> usize {
        let id = self.find_mut(id);
        if self.classes[id.index()].is_none() {
            return 0;
        }
        let parents = std::mem::take(&mut self.class_mut(id).parents);

        // Step 1: drop stale hashcons keys for these parents.
        for (node, _) in &parents {
            self.memo.remove(node);
        }

        // Step 2: re-canonicalize, and union any two parents that have become
        // congruent. Whichever survives, the memo entry is passed through
        // `find` on lookup, so either value is correct.
        let mut unions = 0;
        let mut seen: HashMap<ENode, Id> = HashMap::with_capacity(parents.len());
        for (node, owner) in parents {
            let node = self.canonicalize(&node);
            let owner = self.find_mut(owner);
            match seen.get(&node) {
                Some(&existing) => {
                    if self.union(existing, owner) {
                        unions += 1;
                    }
                    let root = self.find_mut(owner);
                    seen.insert(node.clone(), root);
                    self.memo.insert(node, root);
                }
                None => {
                    seen.insert(node.clone(), owner);
                    self.memo.insert(node, owner);
                }
            }
        }

        let root = self.find_mut(id);
        let new_parents: Vec<(ENode, Id)> = seen.into_iter().collect();
        self.class_mut(root).parents.extend(new_parents);
        unions
    }

    /// Recompute analysis facts until nothing changes, returning the classes
    /// whose fact moved.
    ///
    /// `union` has already merged the two facts into the surviving class, so
    /// recomputing that class from its nodes will agree with what is stored
    /// and report no change. Its *parents* still have to hear about it, which
    /// is why the classes seeded by `union` propagate unconditionally rather
    /// than only when the recomputation moves them.
    fn propagate_analysis(&mut self) -> Vec<Id> {
        if self.analysis_pending.is_empty() {
            return Vec::new();
        }
        let mut seeds: Vec<Id> = std::mem::take(&mut self.analysis_pending)
            .into_iter()
            .map(|i| self.find_mut(i))
            .collect();
        seeds.sort_unstable();
        seeds.dedup();
        let mut forced: HashSet<Id> = seeds.iter().copied().collect();

        let mut in_queue: HashSet<Id> = forced.clone();
        let mut queue: Vec<Id> = seeds;
        let mut changed_any: Vec<Id> = Vec::new();

        while let Some(id) = queue.pop() {
            let id = self.find_mut(id);
            in_queue.remove(&id);
            if self.classes[id.index()].is_none() {
                continue;
            }
            let nodes = self.class(id).nodes.clone();
            let mut acc: Option<A::Data> = None;
            for n in &nodes {
                let n = self.canonicalize(n);
                let d = A::make(self, &n);
                acc = Some(match acc {
                    None => d,
                    Some(prev) => self.analysis.merge(prev, d),
                });
            }
            let Some(new) = acc else { continue };
            let moved = new != self.class(id).data;
            if moved {
                self.class_mut(id).data = new;
            }
            if !moved && !forced.remove(&id) {
                continue;
            }
            forced.remove(&id);
            changed_any.push(id);
            let parents: Vec<Id> = self.class(id).parents.iter().map(|(_, c)| *c).collect();
            for p in parents {
                let p = self.find_mut(p);
                if p != id && in_queue.insert(p) {
                    queue.push(p);
                }
            }
        }
        changed_any.sort_unstable();
        changed_any.dedup();
        changed_any
    }

    /// Rebuild the derived indices — class node lists, the hashcons, and the
    /// parent lists — from scratch.
    ///
    /// `repair` already did the semantically interesting work of discovering
    /// new congruences. This pass is bookkeeping: incremental maintenance of
    /// three indices under interleaved unions is where e-graph implementations
    /// grow their subtlest bugs, and one linear pass per `rebuild` costs far
    /// less than the matching that follows it.
    fn reindex(&mut self) {
        let ids = self.class_ids();

        for &id in &ids {
            let mut nodes = std::mem::take(&mut self.class_mut(id).nodes);
            for n in nodes.iter_mut() {
                *n = self.canonicalize(n);
            }
            nodes.sort();
            nodes.dedup();
            self.class_mut(id).nodes = nodes;
            self.class_mut(id).parents.clear();
        }

        self.memo.clear();
        self.memo.reserve(self.total_nodes());
        let mut parents: Vec<(Id, ENode, Id)> = Vec::new();
        for &id in &ids {
            let nodes = self.classes[id.index()]
                .as_ref()
                .expect("class")
                .nodes
                .clone();
            for n in nodes {
                for &child in &n.children {
                    parents.push((self.find(child), n.clone(), id));
                }
                self.memo.insert(n, id);
            }
        }
        for (child, node, owner) in parents {
            self.classes[child.index()]
                .as_mut()
                .expect("child class")
                .parents
                .push((node, owner));
        }
        for &id in &ids {
            let p = &mut self.classes[id.index()].as_mut().expect("class").parents;
            p.sort();
            p.dedup();
        }
    }

    // -- validation ---------------------------------------------------------

    /// Verify the hashcons and congruence invariants. Panics with a
    /// description of the first violation. Used by tests and `--check`.
    pub fn check_invariants(&self) {
        assert!(
            self.clean,
            "check_invariants called on a dirty e-graph; call rebuild() first"
        );
        // Every occupied slot is canonical, every canonical id is occupied.
        for (i, slot) in self.classes.iter().enumerate() {
            let id = Id::new(i);
            let canon = self.find(id);
            match slot {
                Some(class) => {
                    assert_eq!(canon, id, "class {:?} is stored at a non-canonical id", id);
                    assert_eq!(class.id, id, "class {:?} records the wrong id", id);
                    assert!(!class.nodes.is_empty(), "class {:?} is empty", id);
                }
                None => assert_ne!(canon, id, "canonical id {:?} has no class", id),
            }
        }

        // Every node in every class is canonical and hashconsed to that class.
        let mut node_owner: HashMap<ENode, Id> = HashMap::new();
        for class in self.classes() {
            for n in &class.nodes {
                assert_eq!(
                    *n,
                    self.canonicalize(n),
                    "node {:?} in class {:?} is not canonical",
                    n,
                    class.id
                );
                if let Some(&other) = node_owner.get(n) {
                    assert_eq!(
                        other, class.id,
                        "congruence violated: {:?} appears in classes {:?} and {:?}",
                        n, other, class.id
                    );
                }
                node_owner.insert(n.clone(), class.id);
                let found = self.memo.get(n).unwrap_or_else(|| {
                    panic!("node {:?} of class {:?} missing from memo", n, class.id)
                });
                assert_eq!(
                    self.find(*found),
                    class.id,
                    "hashcons for {:?} points at {:?}, expected {:?}",
                    n,
                    self.find(*found),
                    class.id
                );
            }
        }

        // The memo has no entries for nodes that no class owns.
        for (n, &id) in &self.memo {
            let id = self.find(id);
            assert!(
                self.classes[id.index()]
                    .as_ref()
                    .map(|c| c.nodes.binary_search(n).is_ok())
                    .unwrap_or(false),
                "memo entry {:?} -> {:?} is not owned by that class",
                n,
                id
            );
        }

        // Parent lists are exactly the set of (node, owner) pairs implied by
        // the classes. A linear scan per node would be quadratic on a large
        // graph, so compare the two sets directly.
        let mut expected: HashSet<(Id, ENode, Id)> = HashSet::new();
        for class in self.classes() {
            for n in &class.nodes {
                for &child in &n.children {
                    expected.insert((self.find(child), n.clone(), class.id));
                }
            }
        }
        let mut actual: HashSet<(Id, ENode, Id)> = HashSet::new();
        for class in self.classes() {
            for (n, owner) in &class.parents {
                actual.insert((class.id, n.clone(), self.find(*owner)));
            }
        }
        if let Some(missing) = expected.difference(&actual).next() {
            panic!(
                "class {:?} is missing parent {:?} from class {:?}",
                missing.0, missing.1, missing.2
            );
        }
        if let Some(extra) = actual.difference(&expected).next() {
            panic!(
                "class {:?} lists a stale parent {:?} from class {:?}",
                extra.0, extra.1, extra.2
            );
        }
    }

    // -- output -------------------------------------------------------------

    /// Render the graph as Graphviz DOT, one cluster per e-class.
    pub fn to_dot(&self) -> String {
        let mut s = String::from("digraph egraph {\n");
        s.push_str(
            "  compound=true\n  clusterrank=local\n  node [shape=box, fontname=\"monospace\"]\n",
        );
        for class in self.classes() {
            s.push_str(&format!(
                "  subgraph cluster_{} {{\n    style=dashed\n    label=\"{}\"\n",
                class.id.index(),
                class.id
            ));
            for (i, n) in class.nodes.iter().enumerate() {
                let label = match n.op {
                    Op::Const(c) => c.to_string(),
                    Op::Var(v) => v.to_string(),
                    op => op.name().to_string(),
                };
                s.push_str(&format!(
                    "    n{}_{} [label=\"{}\"]\n",
                    class.id.index(),
                    i,
                    label.replace('"', "\\\"")
                ));
            }
            s.push_str("  }\n");
        }
        for class in self.classes() {
            for (i, n) in class.nodes.iter().enumerate() {
                for (k, &child) in n.children.iter().enumerate() {
                    let c = self.find(child);
                    s.push_str(&format!(
                        "  n{}_{} -> n{}_0 [lhead=cluster_{}, label=\"{}\"]\n",
                        class.id.index(),
                        i,
                        c.index(),
                        c.index(),
                        k
                    ));
                }
            }
        }
        s.push_str("}\n");
        s
    }

    /// A stable, human-readable dump used by tests and `saturn egraph`.
    pub fn dump(&self) -> String {
        let mut s = String::new();
        for class in self.classes() {
            s.push_str(&format!("{}: ", class.id));
            let mut parts: Vec<String> = class.nodes.iter().map(|n| format!("{:?}", n)).collect();
            parts.sort();
            s.push_str(&parts.join(" | "));
            s.push('\n');
        }
        s
    }
}

impl<A: Analysis> Index<Id> for EGraph<A> {
    type Output = EClass<A::Data>;
    fn index(&self, id: Id) -> &EClass<A::Data> {
        self.class(id)
    }
}

impl<A: Analysis> fmt::Debug for EGraph<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "EGraph {{ classes: {}, nodes: {}, clean: {} }}",
            self.number_of_classes(),
            self.total_nodes(),
            self.clean
        )
    }
}
