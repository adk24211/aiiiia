//! Where a saturating run spends its time.
//!
//! Run with `cargo run --release --example profile`. The suite in
//! `saturn bench` measures what the optimizer achieves; this measures what it
//! costs, split by phase, over expressions large enough that the constant
//! factors stop mattering.

use saturn::extract::OpCost;
use saturn::gen::{ExprStream, Grammar};
use saturn::rules;
use saturn::runner::{Runner, StopReason};
use std::time::{Duration, Instant};

const EXPRESSIONS: usize = 200;

fn main() {
    let rules = rules::all_rules();
    let (mut search, mut apply, mut rebuild) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let mut extract = Duration::ZERO;
    let (mut iterations, mut nodes, mut matches, mut truncated) = (0usize, 0usize, 0usize, 0usize);
    let mut stopped_early = 0usize;

    let wall = Instant::now();
    for expr in ExprStream::new(42, Grammar::default(), 6).take(EXPRESSIONS) {
        let runner = Runner::default()
            .with_iter_limit(10)
            .with_node_limit(20_000)
            .with_time_limit(Duration::from_secs(2))
            .with_expr(&expr)
            .run(&rules);

        for it in &runner.iterations {
            search += it.search_time;
            apply += it.apply_time;
            rebuild += it.rebuild_time;
            matches += it.total_matches;
            truncated += it.truncated.len();
            iterations += 1;
        }
        nodes += runner.egraph.total_nodes();
        if !matches!(runner.stop_reason, Some(StopReason::Saturated)) {
            stopped_early += 1;
        }

        let t = Instant::now();
        let _ = saturn::extract::DagExtractor::new(&runner.egraph, OpCost).find_best(runner.root());
        extract += t.elapsed();
    }

    let total = search + apply + rebuild + extract;
    let share = |d: Duration| d.as_secs_f64() / total.as_secs_f64() * 100.0;
    println!(
        "{} expressions, {} iterations, {} e-nodes, {} substitutions",
        EXPRESSIONS, iterations, nodes, matches
    );
    println!(
        "{} runs hit a limit rather than saturating, {} searches were truncated",
        stopped_early, truncated
    );
    println!("wall    {:>10.2?}", wall.elapsed());
    for (name, d) in [
        ("search", search),
        ("apply", apply),
        ("rebuild", rebuild),
        ("extract", extract),
    ] {
        println!("{:<8}{:>10.2?}  {:>4.0}%", name, d, share(d));
    }
}
