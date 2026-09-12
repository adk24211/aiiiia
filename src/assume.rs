//! Facts the caller supplies about the variables.
//!
//! The interval analysis can only prove what the expression itself implies,
//! and that is usually nothing: `?x / ?x => 1` never fires on a bare variable,
//! because `x` might be zero, infinite, or NaN. An assumption is how the
//! person who knows better says so, and it is the difference between a safe
//! rule tier that is correct and one that is also useful.
//!
//! ```text
//! x > 0
//! -1 <= t, t <= 1
//! finite(w) && nonzero(w)
//! ```
//!
//! Assumptions are taken on trust. They widen what the safe tier will do, so
//! an assumption that is false makes the result wrong in exactly the way a
//! fast-math rule would — which is why they have to be asked for.

use crate::interval::Interval;
use crate::lexer::{lex, ParseError, Tok};
use crate::sym::Sym;
use std::collections::HashMap;

/// What is assumed about each variable, merged across every constraint given.
pub type Assumptions = HashMap<Sym, Interval>;

/// Parse a comma- or `&&`-separated list of constraints.
pub fn parse(src: &str) -> Result<Assumptions, ParseError> {
    let tokens = lex(src)?;
    let mut out: Assumptions = HashMap::new();
    let mut i = 0usize;

    let at = |i: usize| &tokens[i.min(tokens.len() - 1)];
    while at(i).kind != Tok::Eof {
        let start = i;
        let (var, constraint) = constraint(src, &tokens, &mut i)?;
        if i == start {
            return Err(ParseError::new(
                "expected a constraint",
                at(i).start,
                at(i).end,
                src,
            ));
        }
        out.entry(var)
            .and_modify(|existing| *existing = existing.meet(constraint))
            .or_insert(constraint);

        match at(i).kind {
            Tok::Eof => break,
            Tok::Comma | Tok::AndAnd => i += 1,
            _ => {
                return Err(ParseError::new(
                    "expected `,` or `&&` between constraints",
                    at(i).start,
                    at(i).end,
                    src,
                ))
            }
        }
    }
    Ok(out)
}

/// Parse one constraint, advancing `i` past it.
fn constraint(
    src: &str,
    tokens: &[crate::lexer::Token],
    i: &mut usize,
) -> Result<(Sym, Interval), ParseError> {
    let at = |i: usize| &tokens[i.min(tokens.len() - 1)];

    // `finite(x)` and `nonzero(x)` are predicates rather than comparisons:
    // neither is a range, and `nonzero` is not one at all -- it is a hole in
    // the middle of one, which is why the domain carries it as its own fact.
    if at(*i).kind == Tok::Ident && at(*i + 1).kind == Tok::LParen {
        let name = at(*i).text.clone();
        let interval = match name.as_str() {
            "finite" => Interval::new(f64::MIN, f64::MAX),
            "nonzero" => Interval::TOP.meet(Interval::REAL).nonzero(),
            _ => {
                return Err(ParseError::new(
                    format!("unknown predicate `{}`; expected finite or nonzero", name),
                    at(*i).start,
                    at(*i).end,
                    src,
                ))
            }
        };
        *i += 2;
        if at(*i).kind != Tok::Ident {
            return Err(ParseError::new(
                format!("`{}` takes a variable name", name),
                at(*i).start,
                at(*i).end,
                src,
            ));
        }
        let var = Sym::new(&at(*i).text);
        *i += 1;
        if at(*i).kind != Tok::RParen {
            return Err(ParseError::new(
                "expected `)`",
                at(*i).start,
                at(*i).end,
                src,
            ));
        }
        *i += 1;
        return Ok((var, interval));
    }

    // Otherwise a comparison, with the variable on either side.
    let (var, op, value, flipped) = if at(*i).kind == Tok::Ident {
        let var = Sym::new(&at(*i).text);
        *i += 1;
        let op = comparison(src, tokens, i)?;
        let value = number(src, tokens, i)?;
        (var, op, value, false)
    } else {
        let value = number(src, tokens, i)?;
        let op = comparison(src, tokens, i)?;
        if at(*i).kind != Tok::Ident {
            return Err(ParseError::new(
                "expected a variable name",
                at(*i).start,
                at(*i).end,
                src,
            ));
        }
        let var = Sym::new(&at(*i).text);
        *i += 1;
        (var, op, value, true)
    };

    // `3 < x` is `x > 3`.
    let op = if flipped { mirror(op) } else { op };
    let interval = match op {
        Tok::Gt => Interval::new(next_above(value), f64::INFINITY),
        Tok::Ge => Interval::new(value, f64::INFINITY),
        Tok::Lt => Interval::new(f64::NEG_INFINITY, next_below(value)),
        Tok::Le => Interval::new(f64::NEG_INFINITY, value),
        Tok::EqEq => Interval::point(value),
        Tok::Ne if value == 0.0 => Interval::REAL.nonzero(),
        Tok::Ne => {
            return Err(ParseError::new(
                "`!=` is only understood against 0; an interval cannot exclude \
                 a value from the middle of a range",
                at(*i - 1).start,
                at(*i - 1).end,
                src,
            ))
        }
        _ => unreachable!("comparison() returns only comparison tokens"),
    };
    Ok((var, interval))
}

fn comparison(src: &str, tokens: &[crate::lexer::Token], i: &mut usize) -> Result<Tok, ParseError> {
    let t = &tokens[(*i).min(tokens.len() - 1)];
    match t.kind {
        Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge | Tok::EqEq | Tok::Ne => {
            *i += 1;
            Ok(t.kind)
        }
        _ => Err(ParseError::new(
            "expected a comparison: <, <=, >, >=, == or !=",
            t.start,
            t.end,
            src,
        )),
    }
}

fn number(src: &str, tokens: &[crate::lexer::Token], i: &mut usize) -> Result<f64, ParseError> {
    let mut sign = 1.0;
    let mut t = &tokens[(*i).min(tokens.len() - 1)];
    if t.kind == Tok::Minus {
        sign = -1.0;
        *i += 1;
        t = &tokens[(*i).min(tokens.len() - 1)];
    }
    match t.kind {
        Tok::Num => {
            let v: f64 = t.text.parse().map_err(|_| {
                ParseError::new(format!("`{}` is not a number", t.text), t.start, t.end, src)
            })?;
            *i += 1;
            Ok(sign * v)
        }
        Tok::Ident if t.text == "inf" => {
            *i += 1;
            Ok(sign * f64::INFINITY)
        }
        Tok::Ident if crate::parser::named_constant(&t.text).is_some() => {
            let v = crate::parser::named_constant(&t.text).expect("just checked");
            *i += 1;
            Ok(sign * v)
        }
        _ => Err(ParseError::new("expected a number", t.start, t.end, src)),
    }
}

fn mirror(op: Tok) -> Tok {
    match op {
        Tok::Lt => Tok::Gt,
        Tok::Le => Tok::Ge,
        Tok::Gt => Tok::Lt,
        Tok::Ge => Tok::Le,
        other => other,
    }
}

/// The smallest double strictly greater than `x`.
///
/// `x > 3` bounds the value below by the next double after 3, not by 3: an
/// interval's bounds are inclusive, so using 3 would admit exactly the value
/// the constraint rules out — which for `x > 0` is the one that matters.
fn next_above(x: f64) -> f64 {
    if x.is_infinite() {
        x
    } else {
        x.next_up()
    }
}

fn next_below(x: f64) -> f64 {
    if x.is_infinite() {
        x
    } else {
        x.next_down()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(src: &str) -> Interval {
        let a = parse(src).unwrap_or_else(|e| panic!("{}", e));
        assert_eq!(a.len(), 1, "expected one variable in `{}`", src);
        *a.values().next().expect("one entry")
    }

    #[test]
    fn comparisons_become_bounds() {
        assert!(one("x > 0").is_positive());
        assert!(!one("x >= 0").is_positive(), "zero is still admitted");
        assert!(one("x >= 0").is_nonneg());
        assert!(one("x < 0").is_negative());
        assert!(one("x <= 0").is_nonpos());
        assert_eq!(one("x == 2.5").as_constant(), Some(2.5));
    }

    #[test]
    fn the_variable_may_be_on_either_side() {
        assert_eq!(one("3 < x"), one("x > 3"));
        assert_eq!(one("3 >= x"), one("x <= 3"));
        assert_eq!(one("0 == x"), one("x == 0"));
    }

    #[test]
    fn a_strict_bound_excludes_the_value_itself() {
        // The whole point of `x > 0` is that zero is out.
        let i = one("x > 0");
        assert!(i.lo > 0.0);
        assert!(i.is_nonzero());
        assert!(one("x >= 0").lo == 0.0);
    }

    #[test]
    fn predicates_say_what_a_range_cannot() {
        assert!(one("finite(x)").is_finite());
        assert!(!one("finite(x)").is_nonzero());
        assert!(one("nonzero(x)").is_nonzero());
        assert!(
            !one("nonzero(x)").is_finite(),
            "non-zero says nothing about size"
        );
    }

    #[test]
    fn constraints_combine() {
        let a = parse("finite(x) && nonzero(x)").unwrap();
        assert!(a[&Sym::new("x")].is_finite_nonzero());
        let a = parse("-1 <= t, t <= 1").unwrap();
        let t = a[&Sym::new("t")];
        assert_eq!((t.lo, t.hi), (-1.0, 1.0));
        let a = parse("x > 0 && y < 0").unwrap();
        assert_eq!(a.len(), 2);
        assert!(a[&Sym::new("x")].is_positive());
        assert!(a[&Sym::new("y")].is_negative());
    }

    #[test]
    fn negative_and_named_values_parse() {
        assert_eq!(one("x >= -2.5").lo, -2.5);
        assert_eq!(one("x <= pi").hi, std::f64::consts::PI);
        assert_eq!(one("x < inf").hi, f64::INFINITY);
    }

    #[test]
    fn nonsense_is_rejected_with_a_span() {
        for src in [
            "x >",
            "> 3",
            "x 3",
            "x > 3 y < 4",
            "unknown(x)",
            "finite(3)",
            "x != 5",
            "",
        ] {
            if src.is_empty() {
                assert!(parse(src).unwrap().is_empty());
                continue;
            }
            let e = parse(src).unwrap_err();
            assert!(e.render().contains('^'), "no span for `{}`", src);
        }
    }

    #[test]
    fn not_equal_to_zero_is_the_one_hole_that_is_allowed() {
        assert!(one("x != 0").is_nonzero());
        let e = parse("x != 5").unwrap_err();
        assert!(e.render().contains("only understood against 0"), "{}", e);
    }
}
