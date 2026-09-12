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
pub mod egraph;
pub mod extract;
pub mod interval;
pub mod lang;
pub mod lexer;
pub mod parser;
pub mod pattern;
pub mod rewrite;
pub mod runner;
pub mod sym;
pub mod unionfind;

pub use analysis::{Analysis, MathAnalysis, MathData, NoAnalysis};
pub use egraph::{EClass, EGraph, EGraphStats};
pub use extract::{AstDepth, AstSize, CostFunction, DagExtractor, Extractor, OpCost};
pub use interval::Interval;
pub use lang::{ENode, Id, Op, RecExpr};
pub use lexer::ParseError;
pub use parser::{parse, parse_rule};
pub use pattern::{Pattern, SearchMatches, Subst};
pub use rewrite::{Applier, ConditionalApplier, DynamicApplier, Rewrite};
pub use runner::{BackoffScheduler, Iteration, Runner, RuleScheduler, SimpleScheduler, StopReason};
pub use sym::{Sym, F};
