//! Rewrite rules: a pattern to look for, and something to do with each match.

use crate::analysis::Analysis;
use crate::egraph::EGraph;
use crate::lang::Id;
use crate::lexer::ParseError;
use crate::parser;
use crate::pattern::{Pattern, SearchMatches, Subst};
use std::fmt;

/// What to add to the e-graph for a given match.
pub trait Applier<A: Analysis> {
    /// Add terms implied by `subst` and union them into `matched`.
    /// Returns the ids of anything newly unioned with `matched`.
    fn apply(&self, egraph: &mut EGraph<A>, matched: Id, subst: &Subst) -> Vec<Id>;

    /// Variables this applier needs bound. Checked when a rule is built.
    fn vars(&self) -> Vec<crate::sym::Sym> {
        Vec::new()
    }

    /// How the right-hand side prints in rule listings.
    fn describe(&self) -> String {
        "<dynamic>".to_string()
    }

    /// The pattern this applier instantiates, when it is a plain pattern,
    /// looking through any conditions wrapped around it.
    ///
    /// Tests use it to check a rule's two sides against each other directly.
    /// A dynamic applier returns `None`, because its result is not a fixed
    /// pattern.
    fn as_pattern(&self) -> Option<&Pattern> {
        None
    }

    /// Whether every side condition on this applier is satisfied.
    ///
    /// Exposed so that a test can ask the real analysis the same question the
    /// rule asks, and check the identity exactly where the rule would fire.
    fn condition_holds(&self, _egraph: &EGraph<A>, _matched: Id, _subst: &Subst) -> bool {
        true
    }
}

/// The ordinary right-hand side: instantiate a pattern.
impl<A: Analysis> Applier<A> for Pattern {
    fn apply(&self, egraph: &mut EGraph<A>, matched: Id, subst: &Subst) -> Vec<Id> {
        let id = self.instantiate(egraph, subst);
        if egraph.union(matched, id) {
            vec![id]
        } else {
            Vec::new()
        }
    }
    fn vars(&self) -> Vec<crate::sym::Sym> {
        // Disambiguate: the inherent `Pattern::vars`, not this trait method.
        Pattern::vars(self).to_vec()
    }
    fn describe(&self) -> String {
        self.to_string_pretty()
    }
    fn as_pattern(&self) -> Option<&Pattern> {
        Some(self)
    }
}

/// A side condition on a match.
pub type Condition<A> = Box<dyn Fn(&EGraph<A>, Id, &Subst) -> bool + Send + Sync>;

/// Wraps an applier so it only fires when `condition` holds.
pub struct ConditionalApplier<A: Analysis> {
    pub condition: Condition<A>,
    pub description: String,
    pub applier: Box<dyn Applier<A> + Send + Sync>,
}

impl<A: Analysis> Applier<A> for ConditionalApplier<A> {
    fn apply(&self, egraph: &mut EGraph<A>, matched: Id, subst: &Subst) -> Vec<Id> {
        if (self.condition)(egraph, matched, subst) {
            self.applier.apply(egraph, matched, subst)
        } else {
            Vec::new()
        }
    }
    fn vars(&self) -> Vec<crate::sym::Sym> {
        self.applier.vars()
    }
    fn describe(&self) -> String {
        format!("{} if {}", self.applier.describe(), self.description)
    }
    fn as_pattern(&self) -> Option<&Pattern> {
        self.applier.as_pattern()
    }
    fn condition_holds(&self, egraph: &EGraph<A>, matched: Id, subst: &Subst) -> bool {
        (self.condition)(egraph, matched, subst)
            && self.applier.condition_holds(egraph, matched, subst)
    }
}

/// An applier built from a closure, for rules a pattern cannot express —
/// expanding `x^3` into `x*x*x`, say, where the exponent is only known at
/// match time.
pub struct DynamicApplier<A: Analysis> {
    #[allow(clippy::type_complexity)]
    pub f: Box<dyn Fn(&mut EGraph<A>, Id, &Subst) -> Vec<Id> + Send + Sync>,
    pub description: String,
}

impl<A: Analysis> Applier<A> for DynamicApplier<A> {
    fn apply(&self, egraph: &mut EGraph<A>, matched: Id, subst: &Subst) -> Vec<Id> {
        (self.f)(egraph, matched, subst)
    }
    fn describe(&self) -> String {
        self.description.clone()
    }
}

/// A named rewrite rule.
pub struct Rewrite<A: Analysis> {
    pub name: String,
    pub searcher: Pattern,
    pub applier: Box<dyn Applier<A> + Send + Sync>,
}

impl<A: Analysis> Rewrite<A> {
    /// Build a rule from a left-hand pattern and an applier.
    ///
    /// Fails if the left-hand side is a bare variable (it would match every
    /// e-class) or if the right-hand side uses a variable the left-hand side
    /// does not bind (it would be unbound at instantiation time).
    pub fn new(
        name: impl Into<String>,
        searcher: Pattern,
        applier: Box<dyn Applier<A> + Send + Sync>,
    ) -> Result<Rewrite<A>, String> {
        let name = name.into();
        if searcher.is_trivial() {
            return Err(format!(
                "rule `{}`: the left-hand side `{}` is a bare variable and matches everything",
                name, searcher
            ));
        }
        let bound = searcher.vars().to_vec();
        for v in applier.vars() {
            if !bound.contains(&v) {
                return Err(format!(
                    "rule `{}`: `{}` appears on the right but is not bound on the left",
                    name, v
                ));
            }
        }
        Ok(Rewrite {
            name,
            searcher,
            applier,
        })
    }

    /// Parse `"lhs => rhs"`.
    pub fn parse(name: impl Into<String>, rule: &str) -> Result<Rewrite<A>, String> {
        let (lhs, rhs, bidir) = parser::parse_rule(rule).map_err(|e: ParseError| e.render())?;
        if bidir {
            return Err(format!(
                "`{}` is bidirectional; use Rewrite::parse_bidirectional",
                rule
            ));
        }
        Rewrite::new(
            name,
            Pattern::from_expr(&lhs),
            Box::new(Pattern::from_expr(&rhs)),
        )
    }

    /// Parse `"lhs <=> rhs"` into the two directed rules it stands for.
    pub fn parse_bidirectional(
        name: impl Into<String>,
        rule: &str,
    ) -> Result<Vec<Rewrite<A>>, String> {
        let name = name.into();
        let (lhs, rhs, _) = parser::parse_rule(rule).map_err(|e: ParseError| e.render())?;
        let lp = Pattern::from_expr(&lhs);
        let rp = Pattern::from_expr(&rhs);
        Ok(vec![
            Rewrite::new(format!("{}-fwd", name), lp.clone(), Box::new(rp.clone()))?,
            Rewrite::new(format!("{}-rev", name), rp, Box::new(lp))?,
        ])
    }

    pub fn search(&self, egraph: &EGraph<A>) -> Vec<SearchMatches> {
        self.searcher.search(egraph)
    }

    /// Apply every match, returning how many e-class unions resulted.
    pub fn apply(&self, egraph: &mut EGraph<A>, matches: &[SearchMatches]) -> usize {
        let mut applied = 0;
        for m in matches {
            for subst in &m.substs {
                applied += self.applier.apply(egraph, m.eclass, subst).len();
            }
        }
        applied
    }

    /// `lhs => rhs`, as written.
    pub fn long_name(&self) -> String {
        format!("{} => {}", self.searcher, self.applier.describe())
    }
}

impl<A: Analysis> fmt::Display for Rewrite<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.long_name())
    }
}

impl<A: Analysis> fmt::Debug for Rewrite<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

/// `rw!("name"; "lhs" => "rhs")` — build a [`Rewrite`], panicking on a
/// malformed rule. Rules are program constants, so a panic here is a bug in
/// the rule text rather than a runtime condition.
#[macro_export]
macro_rules! rw {
    ($name:expr; $lhs:expr => $rhs:expr) => {
        $crate::rewrite::Rewrite::new(
            $name,
            $crate::pattern::Pattern::parse($lhs)
                .unwrap_or_else(|e| panic!("rule `{}` lhs: {}", $name, e)),
            Box::new(
                $crate::pattern::Pattern::parse($rhs)
                    .unwrap_or_else(|e| panic!("rule `{}` rhs: {}", $name, e)),
            ),
        )
        .unwrap_or_else(|e| panic!("{}", e))
    };
    ($name:expr; $lhs:expr => $rhs:expr, if $desc:expr, $cond:expr) => {
        $crate::rewrite::Rewrite::new(
            $name,
            $crate::pattern::Pattern::parse($lhs)
                .unwrap_or_else(|e| panic!("rule `{}` lhs: {}", $name, e)),
            Box::new($crate::rewrite::ConditionalApplier {
                condition: Box::new($cond),
                description: $desc.to_string(),
                applier: Box::new(
                    $crate::pattern::Pattern::parse($rhs)
                        .unwrap_or_else(|e| panic!("rule `{}` rhs: {}", $name, e)),
                ),
            }),
        )
        .unwrap_or_else(|e| panic!("{}", e))
    };
}

/// `rw_bi!("name"; "lhs" <=> "rhs")` — both directions, named `name-fwd` and
/// `name-rev`.
#[macro_export]
macro_rules! rw_bi {
    ($name:expr; $lhs:expr => $rhs:expr) => {{
        let lhs = $crate::pattern::Pattern::parse($lhs)
            .unwrap_or_else(|e| panic!("rule `{}` lhs: {}", $name, e));
        let rhs = $crate::pattern::Pattern::parse($rhs)
            .unwrap_or_else(|e| panic!("rule `{}` rhs: {}", $name, e));
        vec![
            $crate::rewrite::Rewrite::new(
                format!("{}-fwd", $name),
                lhs.clone(),
                Box::new(rhs.clone()),
            )
            .unwrap_or_else(|e| panic!("{}", e)),
            $crate::rewrite::Rewrite::new(format!("{}-rev", $name), rhs, Box::new(lhs))
                .unwrap_or_else(|e| panic!("{}", e)),
        ]
    }};
}
