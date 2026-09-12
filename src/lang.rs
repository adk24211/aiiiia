//! The object language: operators, e-nodes, and flat expression DAGs.

use crate::sym::{Sym, F};
use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Id
// ---------------------------------------------------------------------------

/// An index into an [`EGraph`](crate::EGraph)'s union-find, or into a
/// [`RecExpr`]'s node array.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Id(u32);

impl Id {
    #[inline]
    pub fn new(i: usize) -> Id {
        debug_assert!(i <= u32::MAX as usize, "Id overflow");
        Id(i as u32)
    }
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl From<usize> for Id {
    fn from(i: usize) -> Id {
        Id::new(i)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Op
// ---------------------------------------------------------------------------

/// Every operator in the language.
///
/// Booleans are encoded as floats: `0.0` is false and any other non-NaN value
/// is true. Comparisons and `Not` always produce exactly `0.0` or `1.0`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Op {
    // leaves
    Const(F),
    Var(Sym),

    // arithmetic (binary)
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Min,
    Max,
    Atan2,

    // arithmetic (unary)
    Neg,
    Sqrt,
    Ln,
    Exp,
    Sin,
    Cos,
    Tan,
    Abs,
    Sign,
    Floor,
    Ceil,

    // comparison (binary, boolean-valued)
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,

    // logic
    And,
    Or,
    Not,

    // control
    If,

    /// `d(x, body)` — the symbolic derivative of `body` with respect to the
    /// variable `x`. This is an ordinary node: the rewrite rules in
    /// [`rules::diff`](crate::rules::diff) eliminate it.
    Diff,
}

impl Op {
    /// Number of children this operator takes.
    pub const fn arity(self) -> usize {
        use Op::*;
        match self {
            Const(_) | Var(_) => 0,
            Neg | Sqrt | Ln | Exp | Sin | Cos | Tan | Abs | Sign | Floor | Ceil | Not => 1,
            Add | Sub | Mul | Div | Pow | Min | Max | Atan2 | Lt | Le | Gt | Ge | Eq | Ne | And
            | Or | Diff => 2,
            If => 3,
        }
    }

    /// The surface name, as written by the parser and printer.
    pub fn name(self) -> &'static str {
        use Op::*;
        match self {
            Const(_) => "<const>",
            Var(_) => "<var>",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Pow => "^",
            Min => "min",
            Max => "max",
            Atan2 => "atan2",
            Neg => "neg",
            Sqrt => "sqrt",
            Ln => "ln",
            Exp => "exp",
            Sin => "sin",
            Cos => "cos",
            Tan => "tan",
            Abs => "abs",
            Sign => "sign",
            Floor => "floor",
            Ceil => "ceil",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            Eq => "==",
            Ne => "!=",
            And => "&&",
            Or => "||",
            Not => "!",
            If => "if",
            Diff => "d",
        }
    }

    /// Look up a *function-call* operator by name, e.g. `sqrt` or `atan2`.
    /// Infix operators are produced by the parser directly and are not here.
    pub fn from_fn_name(s: &str) -> Option<Op> {
        use Op::*;
        Some(match s {
            "min" => Min,
            "max" => Max,
            "atan2" => Atan2,
            "neg" => Neg,
            "sqrt" => Sqrt,
            "ln" | "log" => Ln,
            "exp" => Exp,
            "sin" => Sin,
            "cos" => Cos,
            "tan" => Tan,
            "abs" => Abs,
            "sign" => Sign,
            "floor" => Floor,
            "ceil" => Ceil,
            "if" => If,
            "d" | "diff" => Diff,
            "pow" => Pow,
            _ => return None,
        })
    }

    /// True when `f(a, b) == f(b, a)` for all inputs, which lets the matcher
    /// and printer normalize argument order.
    pub const fn is_commutative(self) -> bool {
        use Op::*;
        matches!(self, Add | Mul | Min | Max | Eq | Ne | And | Or)
    }

    /// True if this operator is written infix by the printer.
    pub const fn is_infix(self) -> bool {
        use Op::*;
        matches!(
            self,
            Add | Sub | Mul | Div | Pow | Lt | Le | Gt | Ge | Eq | Ne | And | Or
        )
    }

    /// Binding power for the infix printer and the Pratt parser. Higher binds
    /// tighter. Only meaningful when [`Op::is_infix`] holds.
    pub const fn precedence(self) -> u8 {
        use Op::*;
        match self {
            Or => 1,
            And => 2,
            Eq | Ne => 3,
            Lt | Le | Gt | Ge => 4,
            Add | Sub => 5,
            Mul | Div => 6,
            Pow => 8,
            _ => 10,
        }
    }

    /// `^` is right-associative; every other infix operator is left-associative.
    pub const fn is_right_assoc(self) -> bool {
        matches!(self, Op::Pow)
    }

    /// Evaluate this operator on already-evaluated children.
    ///
    /// Returns `None` when the arity is wrong. Otherwise it always returns a
    /// value — IEEE-754 semantics mean out-of-domain inputs yield NaN or an
    /// infinity rather than an error, and callers (notably constant folding)
    /// are responsible for rejecting non-finite results.
    pub fn eval(self, args: &[f64]) -> Option<f64> {
        use Op::*;
        if args.len() != self.arity() {
            return None;
        }
        let b = |x: bool| if x { 1.0 } else { 0.0 };
        let truthy = |x: f64| x != 0.0 && !x.is_nan();
        Some(match self {
            Const(c) => c.get(),
            Var(_) => return None,
            Add => args[0] + args[1],
            Sub => args[0] - args[1],
            Mul => args[0] * args[1],
            Div => args[0] / args[1],
            Pow => args[0].powf(args[1]),
            Min => args[0].min(args[1]),
            Max => args[0].max(args[1]),
            Atan2 => args[0].atan2(args[1]),
            Neg => -args[0],
            Sqrt => args[0].sqrt(),
            Ln => args[0].ln(),
            Exp => args[0].exp(),
            Sin => args[0].sin(),
            Cos => args[0].cos(),
            Tan => args[0].tan(),
            Abs => args[0].abs(),
            Sign => {
                if args[0].is_nan() {
                    f64::NAN
                } else if args[0] > 0.0 {
                    1.0
                } else if args[0] < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }
            Floor => args[0].floor(),
            Ceil => args[0].ceil(),
            Lt => b(args[0] < args[1]),
            Le => b(args[0] <= args[1]),
            Gt => b(args[0] > args[1]),
            Ge => b(args[0] >= args[1]),
            Eq => b(args[0] == args[1]),
            Ne => b(args[0] != args[1]),
            And => b(truthy(args[0]) && truthy(args[1])),
            Or => b(truthy(args[0]) || truthy(args[1])),
            Not => b(!truthy(args[0])),
            If => {
                if truthy(args[0]) {
                    args[1]
                } else {
                    args[2]
                }
            }
            // `Diff` has no numeric meaning; it must be rewritten away first.
            Diff => return None,
        })
    }

    /// True if evaluating this operator is a pure function of its arguments
    /// *and* safe to perform at compile time. `Diff` and `Var` are not.
    pub const fn is_foldable(self) -> bool {
        !matches!(self, Op::Var(_) | Op::Diff)
    }
}

// ---------------------------------------------------------------------------
// ENode
// ---------------------------------------------------------------------------

/// An operator applied to a list of child ids.
///
/// Inside an e-graph the children are *e-class* ids; inside a [`RecExpr`] they
/// are indices of earlier nodes in the array.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ENode {
    pub op: Op,
    pub children: Vec<Id>,
}

impl ENode {
    pub fn new(op: Op, children: Vec<Id>) -> ENode {
        debug_assert_eq!(
            op.arity(),
            children.len(),
            "arity mismatch building {:?}",
            op
        );
        ENode { op, children }
    }

    pub fn leaf(op: Op) -> ENode {
        ENode::new(op, Vec::new())
    }

    pub fn constant(x: f64) -> ENode {
        ENode::leaf(Op::Const(F::new(x)))
    }

    pub fn var(name: impl Into<Sym>) -> ENode {
        ENode::leaf(Op::Var(name.into()))
    }

    #[inline]
    pub fn children(&self) -> &[Id] {
        &self.children
    }

    #[inline]
    pub fn children_mut(&mut self) -> &mut [Id] {
        &mut self.children
    }

    /// The constant this node holds, if it is a literal.
    #[inline]
    pub fn as_const(&self) -> Option<f64> {
        match self.op {
            Op::Const(c) => Some(c.get()),
            _ => None,
        }
    }

    /// The variable this node names, if it is a variable reference.
    #[inline]
    pub fn as_var(&self) -> Option<Sym> {
        match self.op {
            Op::Var(s) => Some(s),
            _ => None,
        }
    }

    /// Rewrite every child id through `f`, in place.
    pub fn update_children(&mut self, mut f: impl FnMut(Id) -> Id) {
        for c in self.children.iter_mut() {
            *c = f(*c);
        }
    }

    /// A copy of this node with every child id mapped through `f`.
    pub fn map_children(&self, mut f: impl FnMut(Id) -> Id) -> ENode {
        ENode {
            op: self.op,
            children: self.children.iter().map(|&c| f(c)).collect(),
        }
    }

    /// Sort the children of a commutative operator so that `a + b` and `b + a`
    /// hashcons to the same node. This is what makes commutativity free rather
    /// than a rewrite rule that doubles the e-graph.
    pub fn normalize(&mut self) {
        if self.op.is_commutative() && self.children.len() == 2 && self.children[0] > self.children[1]
        {
            self.children.swap(0, 1);
        }
    }
}

impl fmt::Debug for ENode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.op {
            Op::Const(c) => write!(f, "{}", c),
            Op::Var(s) => write!(f, "{}", s),
            op if self.children.is_empty() => write!(f, "({})", op.name()),
            op => {
                write!(f, "({}", op.name())?;
                for c in &self.children {
                    write!(f, " {:?}", c)?;
                }
                write!(f, ")")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RecExpr
// ---------------------------------------------------------------------------

/// A flat, topologically-sorted expression DAG.
///
/// Every node's children have strictly smaller indices, so a single forward
/// pass evaluates the whole expression. Construction through [`RecExpr::add`]
/// hashconses, so structurally identical subterms are shared automatically —
/// the DAG is always maximally shared.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RecExpr {
    nodes: Vec<ENode>,
    memo: HashMap<ENode, Id>,
}

impl RecExpr {
    pub fn new() -> RecExpr {
        RecExpr::default()
    }

    /// Append a node whose children already exist, returning its id. If an
    /// identical node is already present, its existing id is returned instead.
    pub fn add(&mut self, mut node: ENode) -> Id {
        node.normalize();
        for &c in &node.children {
            assert!(
                c.index() < self.nodes.len(),
                "RecExpr::add: child {:?} is not yet in the expression",
                c
            );
        }
        if let Some(&id) = self.memo.get(&node) {
            return id;
        }
        let id = Id::new(self.nodes.len());
        self.memo.insert(node.clone(), id);
        self.nodes.push(node);
        id
    }

    /// Convenience: add `op` applied to `children`.
    pub fn op(&mut self, op: Op, children: Vec<Id>) -> Id {
        self.add(ENode::new(op, children))
    }

    pub fn constant(&mut self, x: f64) -> Id {
        self.add(ENode::constant(x))
    }

    pub fn var(&mut self, name: impl Into<Sym>) -> Id {
        self.add(ENode::var(name))
    }

    #[inline]
    pub fn nodes(&self) -> &[ENode] {
        &self.nodes
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The last node added, which by construction is the root of the DAG.
    #[inline]
    pub fn root(&self) -> Id {
        assert!(!self.nodes.is_empty(), "empty RecExpr has no root");
        Id::new(self.nodes.len() - 1)
    }

    #[inline]
    pub fn node(&self, id: Id) -> &ENode {
        &self.nodes[id.index()]
    }

    /// Number of distinct DAG nodes reachable from `root` — the *shared* size.
    pub fn dag_size(&self) -> usize {
        self.reachable(self.root()).len()
    }

    /// Number of nodes in the fully expanded tree. This is the number the
    /// naive printer emits, and it can be exponentially larger than
    /// [`RecExpr::dag_size`].
    pub fn tree_size(&self) -> u128 {
        let mut sizes: Vec<u128> = Vec::with_capacity(self.nodes.len());
        for n in &self.nodes {
            let s = 1 + n
                .children
                .iter()
                .map(|c| sizes[c.index()])
                .fold(0u128, |a, b| a.saturating_add(b));
            sizes.push(s);
        }
        sizes[self.root().index()]
    }

    /// Ids reachable from `id`, in increasing (topological) order.
    pub fn reachable(&self, id: Id) -> Vec<Id> {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            if seen[x.index()] {
                continue;
            }
            seen[x.index()] = true;
            stack.extend_from_slice(&self.nodes[x.index()].children);
        }
        (0..self.nodes.len())
            .filter(|&i| seen[i])
            .map(Id::new)
            .collect()
    }

    /// How many times each node is referenced by another reachable node.
    pub fn ref_counts(&self, root: Id) -> Vec<u32> {
        let mut counts = vec![0u32; self.nodes.len()];
        for id in self.reachable(root) {
            for &c in &self.nodes[id.index()].children {
                counts[c.index()] += 1;
            }
        }
        counts
    }

    /// Every distinct variable in the expression, sorted by name.
    pub fn vars(&self) -> Vec<Sym> {
        let mut v: Vec<Sym> = self
            .reachable(self.root())
            .into_iter()
            .filter_map(|id| self.nodes[id.index()].as_var())
            .collect();
        v.sort_by_key(|s| s.as_str());
        v.dedup();
        v
    }

    /// Rebuild the expression keeping only what `root` reaches, with `root`
    /// last. Useful after extraction, which can leave dead nodes behind.
    pub fn compact(&self, root: Id) -> RecExpr {
        let mut out = RecExpr::new();
        let mut map: HashMap<Id, Id> = HashMap::new();
        for id in self.reachable(root) {
            let n = &self.nodes[id.index()];
            let new = out.add(ENode::new(
                n.op,
                n.children.iter().map(|c| map[c]).collect(),
            ));
            map.insert(id, new);
        }
        // `reachable` is topological and ends at `root`, so `root` is last.
        debug_assert_eq!(map[&root], out.root());
        out
    }

    /// S-expression form: `(+ (* 2 x) 1)`. Unambiguous, used by tests.
    pub fn to_sexp(&self) -> String {
        self.sexp_of(self.root())
    }

    fn sexp_of(&self, id: Id) -> String {
        let n = self.node(id);
        match n.op {
            Op::Const(c) => format!("{}", c),
            Op::Var(s) => s.to_string(),
            op => {
                let mut s = format!("({}", op.name());
                for &c in &n.children {
                    s.push(' ');
                    s.push_str(&self.sexp_of(c));
                }
                s.push(')');
                s
            }
        }
    }
}

// --- printing ---------------------------------------------------------------

impl RecExpr {
    /// Infix form with minimal parentheses, e.g. `2 * x + 1`.
    ///
    /// Shared subterms are printed once per reference, so this can be much
    /// larger than the DAG. Use [`RecExpr::pretty_shared`] when that matters.
    pub fn pretty(&self) -> String {
        let mut s = String::new();
        self.write_infix(self.root(), 0, &None, &mut s);
        s
    }

    /// Infix form that pulls every subterm used more than once out into a
    /// `let` binding, so the printed text is linear in the DAG size.
    pub fn pretty_shared(&self) -> String {
        let counts = self.ref_counts(self.root());
        let root = self.root();
        // Only bind nodes that are genuinely shared and non-trivial.
        let mut names: HashMap<Id, String> = HashMap::new();
        let mut order: Vec<Id> = Vec::new();
        let mut next = 0usize;
        for id in self.reachable(root) {
            let n = self.node(id);
            let trivial = n.children.is_empty();
            if counts[id.index()] > 1 && !trivial && id != root {
                names.insert(id, format!("t{}", next));
                next += 1;
                order.push(id);
            }
        }
        if order.is_empty() {
            return self.pretty();
        }
        let mut out = String::new();
        for id in &order {
            let mut body = String::new();
            let hide = names.clone();
            let mut hide2 = hide;
            hide2.remove(id); // a binding must not refer to itself
            self.write_infix(*id, 0, &Some(hide2), &mut body);
            out.push_str(&format!("let {} = {} in\n", names[id], body));
        }
        let mut body = String::new();
        self.write_infix(root, 0, &Some(names), &mut body);
        out.push_str(&body);
        out
    }

    fn write_infix(
        &self,
        id: Id,
        parent_prec: u8,
        bound: &Option<HashMap<Id, String>>,
        out: &mut String,
    ) {
        if let Some(map) = bound {
            if let Some(name) = map.get(&id) {
                out.push_str(name);
                return;
            }
        }
        let n = self.node(id);
        match n.op {
            Op::Const(c) => {
                if c.get() < 0.0 {
                    // Keep `2 ^ -1` from lexing back as `2 ^ - 1` ambiguously.
                    out.push('(');
                    out.push_str(&c.to_string());
                    out.push(')');
                } else {
                    out.push_str(&c.to_string());
                }
            }
            Op::Var(s) => out.push_str(s.as_str()),
            Op::Neg => {
                let prec = 7;
                let paren = prec < parent_prec;
                if paren {
                    out.push('(');
                }
                out.push('-');
                self.write_infix(n.children[0], prec, bound, out);
                if paren {
                    out.push(')');
                }
            }
            Op::Not => {
                let prec = 7;
                let paren = prec < parent_prec;
                if paren {
                    out.push('(');
                }
                out.push('!');
                self.write_infix(n.children[0], prec, bound, out);
                if paren {
                    out.push(')');
                }
            }
            op if op.is_infix() => {
                let prec = op.precedence();
                let paren = prec < parent_prec;
                if paren {
                    out.push('(');
                }
                let (lp, rp) = if op.is_right_assoc() {
                    (prec + 1, prec)
                } else {
                    (prec, prec + 1)
                };
                self.write_infix(n.children[0], lp, bound, out);
                out.push(' ');
                out.push_str(op.name());
                out.push(' ');
                self.write_infix(n.children[1], rp, bound, out);
                if paren {
                    out.push(')');
                }
            }
            op => {
                out.push_str(op.name());
                out.push('(');
                for (i, &c) in n.children.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write_infix(c, 0, bound, out);
                }
                out.push(')');
            }
        }
    }
}

impl fmt::Display for RecExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.pretty())
    }
}

impl fmt::Debug for RecExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_sexp())
    }
}
