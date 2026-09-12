//! The reference interpreter.
//!
//! Walks an expression DAG directly, evaluating each node once. This is the
//! ground truth the compiled [`Program`](crate::vm::Program) and every
//! optimized expression are checked against.

use crate::lang::{Id, Op, RecExpr};
use crate::sym::Sym;
use std::collections::HashMap;
use std::fmt;

/// An environment binding variable names to values.
pub type Env = HashMap<Sym, f64>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    /// A variable was used but not bound.
    Unbound(Sym),
    /// A `d(x, e)` node survived to runtime. Differentiation is a rewrite, not
    /// a runtime operation, so this means the rules never fired.
    UnreducedDiff,
    /// The compiled program would need more than 65536 slots or constants.
    /// Slot recycling keeps real programs far below this.
    ProgramTooLarge(&'static str),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::Unbound(s) => write!(f, "`{}` has no value; bind it with -D {}=<num>", s, s),
            EvalError::UnreducedDiff => f.write_str(
                "the expression still contains a `d(...)` that the rules could not eliminate",
            ),
            EvalError::ProgramTooLarge(what) => {
                write!(f, "the expression needs more than 65536 {}", what)
            }
        }
    }
}

impl std::error::Error for EvalError {}

/// Evaluate the root of `expr` under `env`.
pub fn eval(expr: &RecExpr, env: &Env) -> Result<f64, EvalError> {
    eval_at(expr, expr.root(), env)
}

/// Evaluate the node `root` of `expr` under `env`.
///
/// Each reachable node is evaluated exactly once, so sharing in the DAG is
/// sharing of work — an expression whose printed form is astronomically large
/// still evaluates in time linear in its node count.
pub fn eval_at(expr: &RecExpr, root: Id, env: &Env) -> Result<f64, EvalError> {
    let reachable = expr.reachable(root);
    let mut vals: HashMap<Id, f64> = HashMap::with_capacity(reachable.len());
    for id in reachable {
        let n = expr.node(id);
        let v = match n.op {
            Op::Const(c) => c.get(),
            Op::Var(s) => *env.get(&s).ok_or(EvalError::Unbound(s))?,
            Op::Diff => return Err(EvalError::UnreducedDiff),
            op => {
                let args: Vec<f64> = n.children.iter().map(|c| vals[c]).collect();
                op.eval(&args).ok_or(EvalError::UnreducedDiff)?
            }
        };
        vals.insert(id, v);
    }
    Ok(vals[&root])
}

/// Build an environment from `name=value` strings, as the CLI's `-D` takes.
pub fn parse_bindings<'a>(pairs: impl IntoIterator<Item = &'a str>) -> Result<Env, String> {
    let mut env = Env::new();
    for p in pairs {
        let (name, value) = p
            .split_once('=')
            .ok_or_else(|| format!("`{}` should look like `name=value`", p))?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            return Err(format!("`{}` has an empty variable name", p));
        }
        let v: f64 = match value {
            "inf" | "+inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            "nan" => f64::NAN,
            _ => value
                .parse()
                .map_err(|_| format!("`{}` is not a number (in `{}`)", value, p))?,
        };
        env.insert(Sym::new(name), v);
    }
    Ok(env)
}
