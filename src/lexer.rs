//! Tokenizer for the surface syntax.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Num,
    Ident,
    /// A pattern variable such as `?x`.
    PatVar,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
    Comma,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    Bang,
    Eq,
    Arrow,
    BiArrow,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Tok,
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// A syntax error, with the byte span that caused it.
#[derive(Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub start: usize,
    pub end: usize,
    pub source: String,
}

impl ParseError {
    pub fn new(message: impl Into<String>, start: usize, end: usize, source: &str) -> ParseError {
        ParseError {
            message: message.into(),
            start,
            end: end.max(start + 1),
            source: source.to_owned(),
        }
    }

    /// The error with the offending text underlined, ready to print.
    pub fn render(&self) -> String {
        let line_start = self.source[..self.start.min(self.source.len())]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let line_end = self.source[line_start..]
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(self.source.len());
        let line = &self.source[line_start..line_end];
        let col = self.start.saturating_sub(line_start);
        let width = (self.end - self.start).max(1).min(line.len().saturating_sub(col).max(1));
        format!(
            "parse error: {}\n  {}\n  {}{}",
            self.message,
            line,
            " ".repeat(col),
            "^".repeat(width)
        )
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl fmt::Debug for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl std::error::Error for ParseError {}

pub fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    let push = |out: &mut Vec<Token>, kind, start, end| {
        out.push(Token {
            kind,
            text: src[start..end].to_owned(),
            start,
            end,
        })
    };

    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // `#` to end of line is a comment.
        if c == '#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;

        if c.is_ascii_digit() || (c == '.' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit())
        {
            i += 1;
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            // Exponent part, e.g. `1e-9`.
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let save = i;
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                if i < b.len() && (b[i] as char).is_ascii_digit() {
                    while i < b.len() && (b[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save; // `2e` is the number 2 followed by the name `e`.
                }
            }
            let text = &src[start..i];
            if text.parse::<f64>().is_err() {
                return Err(ParseError::new(
                    format!("`{}` is not a valid number", text),
                    start,
                    i,
                    src,
                ));
            }
            push(&mut out, Tok::Num, start, i);
            continue;
        }

        if c == '?' {
            i += 1;
            let name_start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if i == name_start {
                return Err(ParseError::new(
                    "`?` must be followed by a pattern variable name",
                    start,
                    i,
                    src,
                ));
            }
            push(&mut out, Tok::PatVar, start, i);
            continue;
        }

        if c.is_ascii_alphabetic() || c == '_' {
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            push(&mut out, Tok::Ident, start, i);
            continue;
        }

        let two = if i + 1 < b.len() {
            &src[i..i + 2]
        } else {
            ""
        };
        let three = if i + 2 < b.len() { &src[i..i + 3] } else { "" };

        if three == "<=>" {
            i += 3;
            push(&mut out, Tok::BiArrow, start, i);
            continue;
        }

        let two_kind = match two {
            "<=" => Some(Tok::Le),
            ">=" => Some(Tok::Ge),
            "==" => Some(Tok::EqEq),
            "!=" => Some(Tok::Ne),
            "&&" => Some(Tok::AndAnd),
            "||" => Some(Tok::OrOr),
            "=>" => Some(Tok::Arrow),
            _ => None,
        };
        if let Some(k) = two_kind {
            i += 2;
            push(&mut out, k, start, i);
            continue;
        }

        let one_kind = match c {
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '^' => Tok::Caret,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' => Tok::Comma,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '!' => Tok::Bang,
            '=' => Tok::Eq,
            _ => {
                return Err(ParseError::new(
                    format!("unexpected character `{}`", c),
                    start,
                    start + c.len_utf8(),
                    src,
                ))
            }
        };
        i += 1;
        push(&mut out, one_kind, start, i);
    }

    out.push(Token {
        kind: Tok::Eof,
        text: String::new(),
        start: src.len(),
        end: src.len(),
    });
    Ok(out)
}
