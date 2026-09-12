//! Several named expressions that share one DAG.
//!
//! Optimizing formulas one at a time throws away the thing they most often
//! have in common: each other. Put them in one e-graph and a subterm two of
//! them use is found once, extracted once, and emitted once.
//!
//! ```text
//! # a rotation, written as two outputs
//! nx = x * cos(t) - y * sin(t)
//! ny = x * sin(t) + y * cos(t)
//! ```
//!
//! Here `cos(t)` and `sin(t)` are each computed once for both outputs, which
//! is the difference between two transcendental calls and four.

use crate::extract::CostFunction;
use crate::lang::{ENode, Id, RecExpr};
use crate::lexer::ParseError;
use crate::parser;
use crate::sym::Sym;
use std::collections::HashMap;

/// Named expressions sharing one expression DAG.
#[derive(Clone, Debug, Default)]
pub struct Bundle {
    pub expr: RecExpr,
    /// Output name and the node that computes it, in source order.
    pub outputs: Vec<(String, Id)>,
}

impl Bundle {
    /// Parse one expression per line, each optionally named with `name =`.
    ///
    /// Blank lines and `#` comments are skipped. Unnamed expressions are
    /// called `out0`, `out1`, and so on.
    pub fn parse(src: &str) -> Result<Bundle, ParseError> {
        let mut bundle = Bundle::default();
        for (offset, line) in line_spans(src) {
            let trimmed = strip_comment(line).trim();
            if trimmed.is_empty() {
                continue;
            }
            let (name, body, body_offset) = match split_binding(trimmed) {
                Some((name, body)) => {
                    let at = trimmed.len() - body.len();
                    (name.to_string(), body, offset + at)
                }
                None => (format!("out{}", bundle.outputs.len()), trimmed, offset),
            };
            // Parse the body alone so an error points at the user's text
            // rather than at an offset inside a reassembled line.
            let one = parser::parse(body).map_err(|e| ParseError {
                start: e.start + body_offset,
                end: e.end + body_offset,
                source: src.to_string(),
                ..e
            })?;
            let id = bundle.expr.extend_from(&one, one.root());
            bundle.outputs.push((name, id));
        }
        Ok(bundle)
    }

    /// A bundle holding one unnamed expression.
    pub fn single(expr: RecExpr) -> Bundle {
        let root = expr.root();
        Bundle {
            expr,
            outputs: vec![("out".to_string(), root)],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    pub fn roots(&self) -> Vec<Id> {
        self.outputs.iter().map(|(_, id)| *id).collect()
    }

    /// Every variable any output reads, sorted by name.
    pub fn vars(&self) -> Vec<Sym> {
        let mut v: Vec<Sym> = self
            .reachable()
            .into_iter()
            .filter_map(|id| self.expr.node(id).as_var())
            .collect();
        v.sort_by_key(|s| s.as_str());
        v.dedup();
        v
    }

    /// Nodes any output depends on, in dependency order.
    pub fn reachable(&self) -> Vec<Id> {
        let mut seen = vec![false; self.expr.len()];
        let mut stack = self.roots();
        while let Some(x) = stack.pop() {
            if seen[x.index()] {
                continue;
            }
            seen[x.index()] = true;
            stack.extend_from_slice(self.expr.node(x).children());
        }
        (0..self.expr.len())
            .filter(|&i| seen[i])
            .map(Id::new)
            .collect()
    }

    /// Distinct nodes across every output — what actually gets computed.
    pub fn dag_size(&self) -> usize {
        self.reachable().len()
    }

    /// Total cost counting each shared node once.
    pub fn cost<C: CostFunction>(&self, cost_fn: &C) -> f64 {
        crate::extract::dag_cost_of(&self.expr, &self.reachable(), cost_fn)
    }

    /// The sum of what the outputs would cost if each were built alone.
    ///
    /// The gap between this and [`Bundle::cost`] is what sharing is worth.
    pub fn cost_apart<C: CostFunction>(&self, cost_fn: &C) -> f64 {
        self.outputs
            .iter()
            .map(|(_, id)| {
                let one = self.expr.compact(*id);
                crate::extract::dag_cost(&one, cost_fn)
            })
            .sum()
    }

    /// Each output on its own, for printing or checking one at a time.
    pub fn parts(&self) -> Vec<(String, RecExpr)> {
        self.outputs
            .iter()
            .map(|(name, id)| (name.clone(), self.expr.compact(*id)))
            .collect()
    }
}

impl RecExpr {
    /// Copy the nodes `other` reaches from `root` into this expression,
    /// returning where the root landed.
    ///
    /// Nodes are hashconsed on the way in, so anything already present is
    /// shared rather than duplicated — which is how several separately parsed
    /// expressions come to share one DAG.
    pub fn extend_from(&mut self, other: &RecExpr, root: Id) -> Id {
        let mut map: HashMap<Id, Id> = HashMap::new();
        let mut last = None;
        for id in other.reachable(root) {
            let n = other.node(id);
            let new = self.add(ENode::new(
                n.op,
                n.children().iter().map(|c| map[c]).collect::<Vec<_>>(),
            ));
            map.insert(id, new);
            last = Some(new);
        }
        last.expect("an expression has at least one node")
    }
}

/// Lines with their byte offsets, so a parse error can point into the source.
fn line_spans(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for line in src.split('\n') {
        out.push((at, line));
        at += line.len() + 1;
    }
    out
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Split `name = body`, but not `a == b` or a comparison.
fn split_binding(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let eq = (0..bytes.len()).find(|&i| {
        bytes[i] == b'='
            && bytes.get(i + 1) != Some(&b'=')
            && (i == 0 || !matches!(bytes[i - 1], b'=' | b'!' | b'<' | b'>'))
    })?;
    let name = line[..eq].trim();
    let body = line[eq + 1..].trim();
    let is_name = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (is_name && !body.is_empty()).then_some((name, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::AstSize;

    #[test]
    fn outputs_are_named_or_numbered() {
        let b = Bundle::parse("p = a + b\na - b\nq = a * b").unwrap();
        let names: Vec<&str> = b.outputs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["p", "out1", "q"]);
    }

    #[test]
    fn blank_lines_and_comments_are_skipped() {
        let b = Bundle::parse("# a rotation\n\np = a + b   # the sum\n\n").unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b.parts()[0].1.pretty(), "a + b");
    }

    #[test]
    fn an_equality_is_not_a_binding() {
        let b = Bundle::parse("a == b\nx <= y").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b.outputs[0].0, "out0");
    }

    #[test]
    fn shared_subterms_are_one_node() {
        let b =
            Bundle::parse("nx = x * cos(t) - y * sin(t)\nny = x * sin(t) + y * cos(t)").unwrap();
        let cosines = b
            .reachable()
            .iter()
            .filter(|&&id| b.expr.node(id).op == crate::lang::Op::Cos)
            .count();
        assert_eq!(cosines, 1, "cos(t) should be computed once");
        // Eight nodes each on their own, eleven between them: the two
        // trigonometric calls and the variables are computed once.
        assert_eq!(b.dag_size(), 11);
        assert_eq!(b.cost(&AstSize), 11.0);
        assert_eq!(b.cost_apart(&AstSize), 16.0);
    }

    #[test]
    fn variables_span_every_output() {
        let b = Bundle::parse("p = a + b\nq = c * d").unwrap();
        let names: Vec<String> = b.vars().iter().map(|s| s.to_string()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn an_error_points_into_the_source() {
        let e = Bundle::parse("p = a + b\nq = a + \nr = 1").unwrap_err();
        let rendered = e.render();
        assert!(rendered.contains("q = a +"), "{}", rendered);
        assert!(rendered.contains('^'), "{}", rendered);
    }

    #[test]
    fn an_empty_source_is_an_empty_bundle() {
        assert!(Bundle::parse("").unwrap().is_empty());
        assert!(Bundle::parse("\n# nothing\n").unwrap().is_empty());
    }
}
