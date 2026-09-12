//! The saturation loop.
//!
//! Each iteration runs in two phases. The *search* phase matches every rule
//! against a frozen e-graph; the *apply* phase adds the right-hand sides. They
//! are separated so that a rule cannot match a term another rule created in
//! the same iteration, which keeps a run reproducible and stops a single rule
//! from running away inside one pass.

use crate::analysis::Analysis;
use crate::egraph::EGraph;
use crate::lang::{Id, RecExpr};
use crate::pattern::SearchMatches;
use crate::rewrite::Rewrite;
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

/// Why the runner stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// No rule found anything new: the e-graph is *saturated*, and it now
    /// represents every term the rules can reach.
    Saturated,
    IterationLimit(usize),
    NodeLimit(usize),
    TimeLimit,
    /// A caller-supplied hook asked to stop.
    Other(String),
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopReason::Saturated => f.write_str("saturated"),
            StopReason::IterationLimit(n) => write!(f, "hit the iteration limit ({})", n),
            StopReason::NodeLimit(n) => write!(f, "hit the node limit ({})", n),
            StopReason::TimeLimit => f.write_str("hit the time limit"),
            StopReason::Other(s) => f.write_str(s),
        }
    }
}

/// What happened during one pass over the rules.
#[derive(Clone, Debug, Default)]
pub struct Iteration {
    pub index: usize,
    pub classes_before: usize,
    pub nodes_before: usize,
    pub classes_after: usize,
    pub nodes_after: usize,
    /// Rule name -> number of unions it caused.
    pub applied: BTreeMap<String, usize>,
    pub total_matches: usize,
    pub rebuild_unions: usize,
    pub banned: Vec<String>,
    pub search_time: Duration,
    pub apply_time: Duration,
    pub rebuild_time: Duration,
}

impl Iteration {
    pub fn total_time(&self) -> Duration {
        self.search_time + self.apply_time + self.rebuild_time
    }
    pub fn grew(&self) -> bool {
        self.classes_after != self.classes_before || self.nodes_after != self.nodes_before
    }
}

/// Decides which rules to run each iteration.
pub trait RuleScheduler<A: Analysis> {
    fn search(
        &mut self,
        iteration: usize,
        egraph: &EGraph<A>,
        rule: &Rewrite<A>,
    ) -> Vec<SearchMatches>;

    /// Rules the scheduler suppressed this iteration, for the report.
    fn banned(&self) -> Vec<String> {
        Vec::new()
    }

    /// Called when the rules found nothing. Returning `false` keeps the loop
    /// running — a backoff scheduler uses this to unban rules and try again
    /// rather than declaring a premature saturation.
    fn can_stop(&mut self, _iteration: usize) -> bool {
        true
    }
}

/// Runs every rule every iteration.
#[derive(Default, Clone, Copy, Debug)]
pub struct SimpleScheduler;

impl<A: Analysis> RuleScheduler<A> for SimpleScheduler {
    fn search(&mut self, _: usize, egraph: &EGraph<A>, rule: &Rewrite<A>) -> Vec<SearchMatches> {
        rule.search(egraph)
    }
}

#[derive(Clone, Debug)]
struct RuleStats {
    times_applied: usize,
    banned_until: usize,
    times_banned: usize,
    match_limit: usize,
    ban_length: usize,
}

/// Temporarily disables rules that match explosively.
///
/// Associativity and commutativity match a growing number of times each
/// iteration and will crowd out every other rule if left alone. When a rule
/// exceeds its match limit, it is banned for a few iterations and both its
/// limit and its ban length double, so an expensive rule still gets to run —
/// just rarely, and after the cheap rules have shaped the graph.
pub struct BackoffScheduler {
    default_match_limit: usize,
    default_ban_length: usize,
    stats: BTreeMap<String, RuleStats>,
    banned_this_iter: Vec<String>,
    current_iteration: usize,
}

impl Default for BackoffScheduler {
    fn default() -> BackoffScheduler {
        BackoffScheduler {
            default_match_limit: 1_000,
            default_ban_length: 5,
            stats: BTreeMap::new(),
            banned_this_iter: Vec::new(),
            current_iteration: usize::MAX,
        }
    }
}

impl BackoffScheduler {
    pub fn with_match_limit(mut self, n: usize) -> Self {
        self.default_match_limit = n;
        self
    }
    pub fn with_ban_length(mut self, n: usize) -> Self {
        self.default_ban_length = n;
        self
    }

    fn stats_for(&mut self, name: &str) -> &mut RuleStats {
        let (ml, bl) = (self.default_match_limit, self.default_ban_length);
        self.stats.entry(name.to_string()).or_insert(RuleStats {
            times_applied: 0,
            banned_until: 0,
            times_banned: 0,
            match_limit: ml,
            ban_length: bl,
        })
    }
}

impl<A: Analysis> RuleScheduler<A> for BackoffScheduler {
    fn search(
        &mut self,
        iteration: usize,
        egraph: &EGraph<A>,
        rule: &Rewrite<A>,
    ) -> Vec<SearchMatches> {
        // `search` is called once per rule, so the first call of a new
        // iteration is where the previous iteration's ban list is dropped.
        if self.current_iteration != iteration {
            self.current_iteration = iteration;
            self.banned_this_iter.clear();
        }
        {
            let s = self.stats_for(&rule.name);
            if iteration < s.banned_until {
                self.banned_this_iter.push(rule.name.clone());
                return Vec::new();
            }
        }

        let matches = rule.search(egraph);
        let total: usize = matches.iter().map(|m| m.len()).sum();

        let s = self.stats_for(&rule.name);
        let threshold = s.match_limit << s.times_banned.min(16);
        if total > threshold {
            let ban_len = s.ban_length << s.times_banned.min(16);
            s.times_banned += 1;
            s.banned_until = iteration + ban_len;
            self.banned_this_iter.push(rule.name.clone());
            return Vec::new();
        }
        s.times_applied += total;
        matches
    }

    fn banned(&self) -> Vec<String> {
        let mut v = self.banned_this_iter.clone();
        v.sort();
        v.dedup();
        v
    }

    fn can_stop(&mut self, iteration: usize) -> bool {
        // If some rule is still banned, saturation has not really been proven;
        // unban everything and give them one more chance.
        let still_banned: Vec<String> = self
            .stats
            .iter()
            .filter(|(_, s)| s.banned_until > iteration)
            .map(|(n, _)| n.clone())
            .collect();
        if still_banned.is_empty() {
            return true;
        }
        for name in still_banned {
            if let Some(s) = self.stats.get_mut(&name) {
                s.banned_until = iteration;
            }
        }
        false
    }
}

/// Drives equality saturation and records what happened.
pub struct Runner<A: Analysis> {
    pub egraph: EGraph<A>,
    /// The e-classes the caller cares about, in the order they were added.
    pub roots: Vec<Id>,
    pub iterations: Vec<Iteration>,
    pub stop_reason: Option<StopReason>,
    pub iter_limit: usize,
    pub node_limit: usize,
    pub time_limit: Duration,
    scheduler: Box<dyn RuleScheduler<A>>,
    start: Option<Instant>,
}

impl<A: Analysis + Default> Default for Runner<A> {
    fn default() -> Runner<A> {
        Runner::new(A::default())
    }
}

impl<A: Analysis> Runner<A> {
    pub fn new(analysis: A) -> Runner<A> {
        Runner {
            egraph: EGraph::new(analysis),
            roots: Vec::new(),
            iterations: Vec::new(),
            stop_reason: None,
            iter_limit: 30,
            node_limit: 100_000,
            time_limit: Duration::from_secs(10),
            // Backoff by default: a handful of rules that match explosively
            // (association, distribution) will otherwise consume the whole
            // node budget before the rules that actually shrink the expression
            // get a chance to run.
            scheduler: Box::new(BackoffScheduler::default()),
            start: None,
        }
    }

    pub fn with_expr(mut self, expr: &RecExpr) -> Self {
        let id = self.egraph.add_expr(expr);
        self.roots.push(id);
        self
    }

    pub fn with_iter_limit(mut self, n: usize) -> Self {
        self.iter_limit = n;
        self
    }
    pub fn with_node_limit(mut self, n: usize) -> Self {
        self.node_limit = n;
        self
    }
    pub fn with_time_limit(mut self, d: Duration) -> Self {
        self.time_limit = d;
        self
    }
    pub fn with_scheduler(mut self, s: impl RuleScheduler<A> + 'static) -> Self {
        self.scheduler = Box::new(s);
        self
    }

    /// The canonical id of the first root, after saturation.
    pub fn root(&self) -> Id {
        self.egraph
            .find(*self.roots.first().expect("no root expression was added"))
    }

    fn check_limits(&self, iteration: usize) -> Option<StopReason> {
        if iteration >= self.iter_limit {
            return Some(StopReason::IterationLimit(self.iter_limit));
        }
        if self.egraph.total_nodes() > self.node_limit {
            return Some(StopReason::NodeLimit(self.node_limit));
        }
        if let Some(start) = self.start {
            if start.elapsed() > self.time_limit {
                return Some(StopReason::TimeLimit);
            }
        }
        None
    }

    /// Run `rules` until saturation or a limit.
    pub fn run(mut self, rules: &[Rewrite<A>]) -> Self {
        self.start = Some(Instant::now());
        self.egraph.rebuild();

        for i in 0.. {
            if let Some(reason) = self.check_limits(i) {
                self.stop_reason = Some(reason);
                break;
            }

            let mut iter = Iteration {
                index: i,
                classes_before: self.egraph.number_of_classes(),
                nodes_before: self.egraph.total_nodes(),
                ..Default::default()
            };

            // --- search: the e-graph does not change during this phase ---
            let t = Instant::now();
            let mut found: Vec<(usize, Vec<SearchMatches>)> = Vec::with_capacity(rules.len());
            for (ri, rule) in rules.iter().enumerate() {
                let ms = self.scheduler.search(i, &self.egraph, rule);
                iter.total_matches += ms.iter().map(|m| m.len()).sum::<usize>();
                if !ms.is_empty() {
                    found.push((ri, ms));
                }
            }
            iter.search_time = t.elapsed();
            iter.banned = self.scheduler.banned();

            // --- apply ---
            let t = Instant::now();
            let mut unions = 0;
            for (ri, ms) in &found {
                let n = rules[*ri].apply(&mut self.egraph, ms);
                if n > 0 {
                    *iter.applied.entry(rules[*ri].name.clone()).or_insert(0) += n;
                }
                unions += n;
                // A rule that explodes mid-apply should not be allowed to blow
                // past the node limit before the next check.
                if self.egraph.total_nodes() > self.node_limit.saturating_mul(2) {
                    break;
                }
            }
            iter.apply_time = t.elapsed();

            // --- rebuild ---
            let t = Instant::now();
            iter.rebuild_unions = self.egraph.rebuild();
            iter.rebuild_time = t.elapsed();

            iter.classes_after = self.egraph.number_of_classes();
            iter.nodes_after = self.egraph.total_nodes();
            let progressed = unions > 0 || iter.grew();
            self.iterations.push(iter);

            if !progressed && self.scheduler.can_stop(i) {
                self.stop_reason = Some(StopReason::Saturated);
                break;
            }
        }

        if self.stop_reason.is_none() {
            self.stop_reason = Some(StopReason::Saturated);
        }
        self.egraph.rebuild();
        self
    }

    pub fn elapsed(&self) -> Duration {
        self.iterations.iter().map(|i| i.total_time()).sum()
    }

    /// A human-readable summary of the run.
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "stopped: {}\niterations: {}\nclasses: {}\nnodes: {}\ntotal time: {:.2?}\n",
            self.stop_reason
                .as_ref()
                .map(|r| r.to_string())
                .unwrap_or_else(|| "not run".into()),
            self.iterations.len(),
            self.egraph.number_of_classes(),
            self.egraph.total_nodes(),
            self.elapsed(),
        ));
        let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
        for it in &self.iterations {
            for (name, n) in &it.applied {
                *totals.entry(name.as_str()).or_insert(0) += n;
            }
        }
        if !totals.is_empty() {
            let mut rows: Vec<(&str, usize)> = totals.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            s.push_str("rules that fired:\n");
            for (name, n) in rows.iter().take(20) {
                s.push_str(&format!("  {:>6}  {}\n", n, name));
            }
            if rows.len() > 20 {
                s.push_str(&format!("  ... and {} more\n", rows.len() - 20));
            }
        }
        s
    }
}
