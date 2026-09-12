//! A Pratt parser for the surface syntax.
//!
//! ```text
//! e ::= number | ident | pi
//!     | ?ident                        pattern variable
//!     | ident '(' e,* ')'             call: sqrt, min, if, d, ...
//!     | '-' e | '!' e
//!     | e ('+'|'-'|'*'|'/'|'^') e
//!     | e ('<'|'<='|'>'|'>='|'=='|'!=') e
//!     | e ('&&'|'||') e
//!     | 'let' ident '=' e 'in' e      inlined at parse time
//!     | '(' e ')'
//! ```
//!
//! `let` is substituted away immediately. The language is pure, so inlining
//! changes nothing semantically, and [`RecExpr`] hashconses the result — the
//! sharing a `let` expressed is recovered exactly, without the e-graph ever
//! needing to reason about binders.

use crate::lang::{Id, Op, RecExpr};
use crate::lexer::{lex, ParseError, Tok, Token};
use crate::sym::Sym;
use std::collections::HashMap;

pub struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
    expr: RecExpr,
    /// `let`-bound names in scope, innermost last.
    scope: Vec<(Sym, Id)>,
}

/// Parse a single expression.
pub fn parse(src: &str) -> Result<RecExpr, ParseError> {
    let mut p = Parser::new(src)?;
    let root = p.parse_expr(0)?;
    p.expect(Tok::Eof)?;
    Ok(p.expr.compact(root))
}

/// Parse a rewrite rule of the form `lhs => rhs` or `lhs <=> rhs`.
///
/// Returns the two sides and whether the rule is bidirectional.
pub fn parse_rule(src: &str) -> Result<(RecExpr, RecExpr, bool), ParseError> {
    let mut p = Parser::new(src)?;
    let lhs = p.parse_expr(0)?;
    let bidir = match p.peek().kind {
        Tok::Arrow => {
            p.bump();
            false
        }
        Tok::BiArrow => {
            p.bump();
            true
        }
        _ => {
            let t = p.peek().clone();
            return Err(p.err("expected `=>` or `<=>` between the sides of a rule", &t));
        }
    };
    let rhs = p.parse_expr(0)?;
    p.expect(Tok::Eof)?;
    let left = p.expr.compact(lhs);
    let right = p.expr.compact(rhs);
    Ok((left, right, bidir))
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Result<Parser<'a>, ParseError> {
        Ok(Parser {
            src,
            toks: lex(src)?,
            pos: 0,
            expr: RecExpr::new(),
            scope: Vec::new(),
        })
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn err(&self, msg: impl Into<String>, t: &Token) -> ParseError {
        ParseError::new(msg, t.start, t.end, self.src)
    }

    fn expect(&mut self, kind: Tok) -> Result<Token, ParseError> {
        if self.peek().kind == kind {
            Ok(self.bump())
        } else {
            let t = self.peek().clone();
            let what = match kind {
                Tok::RParen => "`)`".to_string(),
                Tok::LParen => "`(`".to_string(),
                Tok::Comma => "`,`".to_string(),
                Tok::Eq => "`=`".to_string(),
                Tok::Ident => "a name".to_string(),
                Tok::Eof => "end of input".to_string(),
                k => format!("{:?}", k),
            };
            let got = if t.kind == Tok::Eof {
                "end of input".to_string()
            } else {
                format!("`{}`", t.text)
            };
            Err(self.err(format!("expected {}, found {}", what, got), &t))
        }
    }

    fn infix_op(kind: Tok) -> Option<Op> {
        Some(match kind {
            Tok::Plus => Op::Add,
            Tok::Minus => Op::Sub,
            Tok::Star => Op::Mul,
            Tok::Slash => Op::Div,
            Tok::Caret => Op::Pow,
            Tok::Lt => Op::Lt,
            Tok::Le => Op::Le,
            Tok::Gt => Op::Gt,
            Tok::Ge => Op::Ge,
            Tok::EqEq => Op::Eq,
            Tok::Ne => Op::Ne,
            Tok::AndAnd => Op::And,
            Tok::OrOr => Op::Or,
            _ => return None,
        })
    }

    /// Pratt loop: parse a prefix, then absorb infix operators that bind at
    /// least as tightly as `min_prec`.
    fn parse_expr(&mut self, min_prec: u8) -> Result<Id, ParseError> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let Some(op) = Self::infix_op(self.peek().kind) else {
                break;
            };
            let prec = op.precedence();
            if prec < min_prec {
                break;
            }
            self.bump();
            let next_min = if op.is_right_assoc() { prec } else { prec + 1 };
            let rhs = self.parse_expr(next_min)?;
            lhs = self.expr.op(op, vec![lhs, rhs]);
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Id, ParseError> {
        // Unary operators bind tighter than every binary operator except `^`,
        // so `-x^2` is `-(x^2)` and `-x*y` is `(-x)*y`.
        const UNARY_PREC: u8 = 7;
        let t = self.peek().clone();
        match t.kind {
            Tok::Minus => {
                self.bump();
                let e = self.parse_expr(UNARY_PREC)?;
                // Fold `-<literal>` into a negative literal so that patterns
                // like `?x ^ -1` have a constant to match on.
                if let Some(c) = self.expr.node(e).as_const() {
                    return Ok(self.expr.constant(-c));
                }
                Ok(self.expr.op(Op::Neg, vec![e]))
            }
            Tok::Plus => {
                self.bump();
                self.parse_expr(UNARY_PREC)
            }
            Tok::Bang => {
                self.bump();
                let e = self.parse_expr(UNARY_PREC)?;
                Ok(self.expr.op(Op::Not, vec![e]))
            }
            Tok::LParen => {
                self.bump();
                let e = self.parse_expr(0)?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::Num => {
                self.bump();
                let v: f64 = t
                    .text
                    .parse()
                    .map_err(|_| self.err(format!("`{}` is not a valid number", t.text), &t))?;
                Ok(self.expr.constant(v))
            }
            Tok::PatVar => {
                self.bump();
                Ok(self.expr.var(t.text.as_str()))
            }
            Tok::Ident => self.parse_ident(),
            Tok::Eof => Err(self.err("unexpected end of input", &t)),
            _ => Err(self.err(format!("unexpected `{}`", t.text), &t)),
        }
    }

    fn parse_ident(&mut self) -> Result<Id, ParseError> {
        let t = self.bump();
        let name = t.text.as_str();

        if name == "let" {
            return self.parse_let();
        }
        if name == "in" {
            return Err(self.err("`in` without a matching `let`", &t));
        }

        // A call: `sqrt(x)`, `min(a, b)`, `if(c, a, b)`, `d(x, e)`.
        if self.peek().kind == Tok::LParen {
            let Some(op) = Op::from_fn_name(name) else {
                return Err(self.err(format!("unknown function `{}`", name), &t));
            };
            self.bump();
            let mut args = Vec::new();
            if self.peek().kind != Tok::RParen {
                loop {
                    args.push(self.parse_expr(0)?);
                    if self.peek().kind == Tok::Comma {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            let close = self.expect(Tok::RParen)?;
            if args.len() != op.arity() {
                return Err(ParseError::new(
                    format!(
                        "`{}` takes {} argument{}, found {}",
                        name,
                        op.arity(),
                        if op.arity() == 1 { "" } else { "s" },
                        args.len()
                    ),
                    t.start,
                    close.end,
                    self.src,
                ));
            }
            if op == Op::Diff {
                let is_var = self.expr.node(args[0]).as_var().is_some();
                if !is_var {
                    return Err(ParseError::new(
                        "the first argument of `d` must be a variable name",
                        t.start,
                        close.end,
                        self.src,
                    ));
                }
            }
            return Ok(self.expr.op(op, args));
        }

        // A `let`-bound name shadows a named constant; innermost binding wins.
        let sym = Sym::new(name);
        if let Some(&(_, id)) = self.scope.iter().rev().find(|(s, _)| *s == sym) {
            return Ok(id);
        }

        if let Some(v) = named_constant(name) {
            return Ok(self.expr.constant(v));
        }

        Ok(self.expr.var(sym))
    }

    fn parse_let(&mut self) -> Result<Id, ParseError> {
        // Any name may be bound, including one that also names a built-in.
        // A name is a call only when it is directly followed by `(`, so
        // `let d = 1 in d + d(x, x)` binds the variable and still calls the
        // derivative operator. Rejecting the binding instead would make `d`
        // unusable as a coefficient name, which is exactly what people write.
        let name_tok = self.expect(Tok::Ident)?;
        self.expect(Tok::Eq)?;
        let bound = self.parse_expr(0)?;
        let in_tok = self.expect(Tok::Ident)?;
        if in_tok.text != "in" {
            return Err(self.err("expected `in` after a `let` binding", &in_tok));
        }
        self.scope.push((Sym::new(&name_tok.text), bound));
        let body = self.parse_expr(0)?;
        self.scope.pop();
        Ok(body)
    }
}

/// Names the parser turns into literals instead of variables.
pub fn named_constant(name: &str) -> Option<f64> {
    Some(match name {
        "pi" | "PI" => std::f64::consts::PI,
        "tau" | "TAU" => std::f64::consts::TAU,
        "inf" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => return None,
    })
}

/// Substitute `bindings` for variables in `expr`, returning a fresh expression.
pub fn substitute(expr: &RecExpr, bindings: &HashMap<Sym, f64>) -> RecExpr {
    let mut out = RecExpr::new();
    let mut map: HashMap<Id, Id> = HashMap::new();
    for id in expr.reachable(expr.root()) {
        let n = expr.node(id);
        let new = match n.as_var() {
            Some(s) if bindings.contains_key(&s) => out.constant(bindings[&s]),
            _ => out.op(n.op, n.children.iter().map(|c| map[c]).collect()),
        };
        map.insert(id, new);
    }
    out.compact(map[&expr.root()])
}
