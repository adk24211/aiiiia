//! Why two expressions are equal.
//!
//! An optimizer that says "these are the same" and cannot say why is asking to
//! be trusted. Every union the e-graph performs carries a reason, and this
//! module walks the chain of reasons connecting two terms back into a
//! derivation a person can check.
//!
//! The chain is a *path*, not a tree. Full e-graph proofs — the kind that
//! recursively justify each congruence step down to the leaves — are a
//! substantially larger construction; this reports the sequence of rule
//! applications, congruences and folds that actually connected the two
//! classes, which is what a reader wants when a rule is under suspicion.

use crate::pattern::Subst;
use crate::sym::Sym;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// A rule, as an explanation refers to it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RuleLabel {
    pub name: String,
    /// The left-hand side, as written.
    pub lhs: String,
    /// The right-hand side, as written.
    pub rhs: String,
}

/// Why two e-classes were merged.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Justification {
    /// A rewrite rule fired on a match.
    Rule {
        rule: Arc<RuleLabel>,
        /// What the pattern variables were bound to. The bindings are e-class
        /// ids, rendered into terms when the explanation is printed.
        subst: Subst,
    },
    /// Two e-nodes turned out to have the same operator and pairwise
    /// equivalent children, so congruence forced them together. The two nodes
    /// are kept so that the step can be unfolded into why each differing
    /// argument is equal.
    Congruence {
        left: crate::lang::ENode,
        right: crate::lang::ENode,
    },
    /// The analysis proved a class equal to a literal.
    Fold,
    /// The caller asserted the equality directly.
    Asserted,
}

impl Justification {
    /// A short label for the step.
    pub fn name(&self) -> &str {
        match self {
            Justification::Rule { rule, .. } => &rule.name,
            Justification::Congruence { .. } => "congruence",
            Justification::Fold => "constant folding",
            Justification::Asserted => "asserted",
        }
    }
}

/// One link in a derivation.
#[derive(Clone, Debug)]
pub struct Step {
    pub justification: Justification,
    /// The two e-classes this step joined, as they were when it happened.
    pub from: crate::lang::Id,
    pub to: crate::lang::Id,
    /// For a congruence step, why each argument that differed is equal. Empty
    /// for every other kind, and empty for a congruence whose sub-derivations
    /// were cut off by the depth limit.
    pub because: Vec<(usize, Explanation)>,
}

/// A derivation connecting two expressions.
#[derive(Clone, Debug, Default)]
pub struct Explanation {
    pub steps: Vec<Step>,
}

impl Explanation {
    pub fn len(&self) -> usize {
        self.steps.len()
    }
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Steps in this derivation and everything nested inside it.
    pub fn total_steps(&self) -> usize {
        self.steps.len()
            + self
                .steps
                .iter()
                .flat_map(|s| &s.because)
                .map(|(_, e)| e.total_steps())
                .sum::<usize>()
    }

    /// How many times each rule appears, nested steps included.
    pub fn rule_counts(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let mut stack: Vec<&Explanation> = vec![self];
        while let Some(e) = stack.pop() {
            for s in &e.steps {
                *counts.entry(s.justification.name()).or_insert(0) += 1;
                stack.extend(s.because.iter().map(|(_, sub)| sub));
            }
        }
        let mut v: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(k, n)| (k.to_string(), n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    /// Render the derivation, using `term` to turn an e-class into text.
    pub fn render(&self, term: &dyn Fn(crate::lang::Id) -> String) -> String {
        let mut out = String::new();
        self.write(term, 0, &mut out);
        out
    }

    fn write(&self, term: &dyn Fn(crate::lang::Id) -> String, depth: usize, out: &mut String) {
        let pad = "    ".repeat(depth);
        if self.steps.is_empty() {
            out.push_str(&format!("{}(the same term)\n", pad));
            return;
        }
        for (i, step) in self.steps.iter().enumerate() {
            let head = format!("{}{}. {}", pad, i + 1, step.justification.name());
            match &step.justification {
                Justification::Rule { rule, subst } => {
                    out.push_str(&format!("{}   {} => {}\n", head, rule.lhs, rule.rhs));
                    let mut vars: Vec<(Sym, crate::lang::Id)> = subst.iter().collect();
                    vars.sort_by_key(|(s, _)| s.as_str());
                    for (v, id) in vars {
                        out.push_str(&format!("{}      {} = {}\n", pad, v, term(id)));
                    }
                }
                Justification::Congruence { left, .. } => {
                    // Printing both nodes would print the same text twice: by
                    // the time anyone reads this the two classes are one, so
                    // extracting a term from each gives the same answer. What
                    // carries the content is the operator and the arguments.
                    out.push_str(&format!(
                        "{}   both sides are `{}` applied to equal arguments{}\n",
                        head,
                        left.op.name(),
                        if step.because.is_empty() { "" } else { ", and" }
                    ));
                    for (index, sub) in &step.because {
                        out.push_str(&format!("{}      argument {}:\n", pad, index + 1));
                        sub.write(term, depth + 2, out);
                    }
                }
                Justification::Fold => {
                    out.push_str(&format!(
                        "{}   the analysis proved it equal to {}\n",
                        head,
                        term(step.to)
                    ));
                }
                Justification::Asserted => out.push_str(&format!("{}   given\n", head)),
            }
        }
    }
}

impl fmt::Display for Explanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, step) in self.steps.iter().enumerate() {
            writeln!(f, "{:>3}. {}", i + 1, step.justification.name())?;
        }
        Ok(())
    }
}
