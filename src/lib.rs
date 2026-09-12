//! `saturn` — equality saturation for a small numeric language.
//!
//! Ordinary compilers rewrite a program in place, one rule at a time, and so
//! must decide *when* to apply each rule. That choice is destructive: applying
//! `x * 2 -> x << 1` can block a later rule that wanted the multiply.
//!
//! Equality saturation avoids the choice. Every rewrite *adds* an equality to
//! an e-graph, which represents an exponential number of equivalent programs
//! in polynomial space. When the rules stop finding anything new — the graph
//! is *saturated* — a cost model picks the best program out of the whole set.
//!
//! ```
//! use saturn::{parse, Runner, Extractor, OpCost, rw, MathAnalysis, Rewrite};
//!
//! let rules: Vec<Rewrite<MathAnalysis>> = vec![
//!     rw!("factor";    "?a * ?b + ?a * ?c" => "?a * (?b + ?c)"),
//!     rw!("cancel";    "?a / ?a"           => "1"),
//!     rw!("mul-1";     "?a * 1"            => "?a"),
//! ];
//!
//! let expr = parse("x * y + x * z").unwrap();
//! let runner = Runner::default().with_expr(&expr).run(&rules);
//! let (_cost, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
//! assert_eq!(best.pretty(), "x * (y + z)");
//! ```

#![forbid(unsafe_code)]

pub mod analysis;
pub mod check;
pub mod codegen;
pub mod egraph;
pub mod eval;
pub mod extract;
pub mod gen;
pub mod interval;
pub mod lang;
pub mod lexer;
pub mod parser;
pub mod pattern;
pub mod rewrite;
pub mod rng;
pub mod rules;
pub mod runner;
pub mod sym;
pub mod unionfind;
pub mod vm;

pub use analysis::{Analysis, MathAnalysis, MathData, NoAnalysis};
pub use check::{Checker, Report};
pub use codegen::{emit, Lang};
pub use egraph::{EClass, EGraph, EGraphStats};
pub use eval::{eval, eval_at, Env, EvalError};
pub use extract::{AstDepth, AstSize, CostFunction, DagExtractor, Extractor, OpCost};
pub use gen::{random_expr, ExprStream, Grammar};
pub use interval::Interval;
pub use lang::{ENode, Id, Op, RecExpr};
pub use lexer::ParseError;
pub use parser::{parse, parse_rule};
pub use pattern::{Pattern, SearchMatches, Subst};
pub use rewrite::{Applier, ConditionalApplier, DynamicApplier, Rewrite};
pub use rng::Rng;
pub use rules::Rule;
pub use runner::{BackoffScheduler, Iteration, RuleScheduler, Runner, SimpleScheduler, StopReason};
pub use sym::{Sym, F};
pub use vm::{Instr, Program};

/// Saturate `expr` with `rules` and return the cheapest equivalent expression.
///
/// The result is never more expensive than the input under `cost_fn`: `expr`
/// is itself in the e-graph, so it is always one of the candidates, and the
/// DAG-aware extractor is a heuristic that can in principle miss it.
///
/// ```
/// use saturn::{optimize, parse, rules, OpCost};
///
/// let expr = parse("u / w + v / w").unwrap();
/// let (best, runner) = optimize(&expr, &rules::all_rules(), OpCost);
/// assert_eq!(best.pretty(), "(u + v) / w");
/// assert!(runner.stop_reason.is_some());
/// ```
pub fn optimize<C: extract::CostFunction>(
    expr: &RecExpr,
    rules: &[Rewrite<MathAnalysis>],
    cost_fn: C,
) -> (RecExpr, Runner<MathAnalysis>) {
    optimize_with(Runner::default(), expr, rules, cost_fn)
}

/// [`optimize`], but with limits and a scheduler you choose.
///
/// The runner must not already have a root expression; `expr` becomes its
/// only one.
pub fn optimize_with<C: extract::CostFunction>(
    runner: Runner<MathAnalysis>,
    expr: &RecExpr,
    rules: &[Rewrite<MathAnalysis>],
    cost_fn: C,
) -> (RecExpr, Runner<MathAnalysis>) {
    let runner = runner.with_expr(expr).run(rules);
    let input_cost = extract::dag_cost(expr, &cost_fn);
    let (cost, best) = extract::DagExtractor::new(&runner.egraph, cost_fn).find_best(runner.root());
    if cost <= input_cost {
        (best, runner)
    } else {
        (expr.clone(), runner)
    }
}
