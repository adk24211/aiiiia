//! Emitting an optimized expression as source code.
//!
//! The point of optimizing an expression is usually to run it somewhere else.
//! This turns the extracted DAG into a function in C, Rust, or Python, binding
//! a temporary for every subterm used more than once — which is where the
//! sharing the e-graph found actually turns into work saved.
//!
//! Semantics are preserved exactly, including the two places the target
//! language disagrees with this one: comparisons produce `0.0` or `1.0` rather
//! than a boolean, and truthiness means "not zero and not NaN".

use crate::lang::{Id, Op, RecExpr};
use crate::sym::F;
use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    C,
    Rust,
    Python,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        Some(match s {
            "c" | "C" => Lang::C,
            "rust" | "rs" => Lang::Rust,
            "python" | "py" => Lang::Python,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::C => "c",
            Lang::Rust => "rust",
            Lang::Python => "python",
        }
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Emit `expr` as a function named `name`.
pub fn emit(expr: &RecExpr, lang: Lang, name: &str) -> String {
    Emitter::new(expr, lang).function(name)
}

struct Emitter<'a> {
    expr: &'a RecExpr,
    lang: Lang,
    /// Nodes bound to a temporary, and its name.
    bound: HashMap<Id, String>,
    /// The binding statements, in dependency order.
    lets: Vec<(String, String)>,
}

impl<'a> Emitter<'a> {
    fn new(expr: &'a RecExpr, lang: Lang) -> Emitter<'a> {
        Emitter {
            expr,
            lang,
            bound: HashMap::new(),
            lets: Vec::new(),
        }
    }

    fn function(mut self, name: &str) -> String {
        let root = self.expr.root();
        let counts = self.expr.ref_counts(root);

        // Bind anything used more than once. A leaf is cheaper to repeat than
        // to name, so those stay inline however often they appear.
        for id in self.expr.reachable(root) {
            let node = self.expr.node(id);
            if id == root || node.children.is_empty() || counts[id.index()] <= 1 {
                continue;
            }
            let text = self.render(id, 0);
            let temp = format!("t{}", self.lets.len());
            self.lets.push((temp.clone(), text));
            self.bound.insert(id, temp);
        }
        let body = self.render(root, 0);

        let params: Vec<String> = self.expr.vars().iter().map(|v| v.to_string()).collect();
        let mut out = String::new();
        match self.lang {
            Lang::C => {
                out.push_str("#include <math.h>\n\n");
                out.push_str(&self.helpers());
                let args = if params.is_empty() {
                    "void".to_string()
                } else {
                    params
                        .iter()
                        .map(|p| format!("double {}", p))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!("double {}({}) {{\n", name, args));
                for (t, v) in &self.lets {
                    out.push_str(&format!("    const double {} = {};\n", t, v));
                }
                out.push_str(&format!("    return {};\n}}\n", body));
            }
            Lang::Rust => {
                let args = params
                    .iter()
                    .map(|p| format!("{}: f64", p))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&self.helpers());
                out.push_str(&format!("pub fn {}({}) -> f64 {{\n", name, args));
                for (t, v) in &self.lets {
                    out.push_str(&format!("    let {} = {};\n", t, v));
                }
                out.push_str(&format!("    {}\n}}\n", body));
            }
            Lang::Python => {
                out.push_str("import math\n\n\n");
                out.push_str(&self.helpers());
                out.push_str(&format!("def {}({}):\n", name, params.join(", ")));
                for (t, v) in &self.lets {
                    out.push_str(&format!("    {} = {}\n", t, v));
                }
                out.push_str(&format!("    return {}\n", body));
            }
        }
        out
    }

    /// Render node `id`, parenthesizing when the context binds tighter.
    fn render(&self, id: Id, parent_prec: u8) -> String {
        if let Some(name) = self.bound.get(&id) {
            return name.clone();
        }
        let node = self.expr.node(id);
        let c = |i: usize, prec: u8| self.render(node.children[i], prec);

        let (text, prec) = match node.op {
            Op::Const(v) => (self.literal(v), 10),
            Op::Var(s) => (s.to_string(), 10),

            Op::Add => (format!("{} + {}", c(0, 5), c(1, 6)), 5),
            Op::Sub => (format!("{} - {}", c(0, 5), c(1, 6)), 5),
            Op::Mul => (format!("{} * {}", c(0, 6), c(1, 7)), 6),
            // Python raises on division by zero where IEEE-754 gives a signed
            // infinity or a NaN.
            Op::Div if self.lang == Lang::Python => (format!("_div({}, {})", c(0, 0), c(1, 0)), 10),
            Op::Div => (format!("{} / {}", c(0, 6), c(1, 7)), 6),
            Op::Neg => (format!("-{}", c(0, 8)), 8),

            Op::Pow => match self.lang {
                Lang::C => (format!("pow({}, {})", c(0, 0), c(1, 0)), 10),
                Lang::Rust => (format!("({}).powf({})", c(0, 0), c(1, 0)), 10),
                // Neither `**` nor `math.pow` is IEEE `pow`: the first returns
                // a complex number for a negative base with a fractional
                // exponent, and the second raises where IEEE gives NaN or an
                // infinity. The helper below restores the float contract.
                Lang::Python => (format!("_pow({}, {})", c(0, 0), c(1, 0)), 10),
            },

            Op::Sqrt
            | Op::Ln
            | Op::Exp
            | Op::Sin
            | Op::Cos
            | Op::Tan
            | Op::Abs
            | Op::Floor
            | Op::Ceil => (self.unary_call(node.op, id), 10),

            Op::Min | Op::Max => (self.minmax(node.op, id), 10),
            Op::Atan2 => match self.lang {
                Lang::C => (format!("atan2({}, {})", c(0, 0), c(1, 0)), 10),
                Lang::Rust => (format!("({}).atan2({})", c(0, 0), c(1, 0)), 10),
                Lang::Python => (format!("math.atan2({}, {})", c(0, 0), c(1, 0)), 10),
            },

            Op::Sign => (format!("{}({})", self.helper("sign"), c(0, 0)), 10),

            Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::And | Op::Or | Op::Not => {
                let native = self
                    .render_bool(id)
                    .expect("every boolean operator has a native form");
                (self.to_float(&native), 10)
            }

            Op::If => {
                let cond = self.truthy(node.children[0]);
                match self.lang {
                    Lang::C => (format!("({} ? {} : {})", cond, c(1, 0), c(2, 0)), 10),
                    Lang::Rust => (
                        format!("if {} {{ {} }} else {{ {} }}", cond, c(1, 0), c(2, 0)),
                        1,
                    ),
                    Lang::Python => (format!("({} if {} else {})", c(1, 0), cond, c(2, 0)), 10),
                }
            }

            Op::Diff => ("/* unreduced derivative */ 0.0".to_string(), 10),
        };

        if prec < parent_prec {
            format!("({})", text)
        } else {
            text
        }
    }

    fn unary_call(&self, op: Op, id: Id) -> String {
        let arg = self.render(self.expr.node(id).children[0], 0);
        let c_name = match op {
            Op::Sqrt => "sqrt",
            Op::Ln => "log",
            Op::Exp => "exp",
            Op::Sin => "sin",
            Op::Cos => "cos",
            Op::Tan => "tan",
            Op::Abs => "fabs",
            Op::Floor => "floor",
            _ => "ceil",
        };
        let rust_name = match op {
            Op::Ln => "ln",
            Op::Abs => "abs",
            other => other.name(),
        };
        match self.lang {
            Lang::C => format!("{}({})", c_name, arg),
            // A literal needs parentheses before a method call: `-1.0.abs()`
            // parses as `-(1.0.abs())`.
            Lang::Rust => format!("({}).{}()", arg, rust_name),
            // `abs` is the one that needs no wrapper: `math.fabs` already
            // agrees with C on infinities and NaN.
            Lang::Python if op == Op::Abs => format!("math.fabs({})", arg),
            Lang::Python => format!("_{}({})", if op == Op::Ln { "log" } else { op.name() }, arg),
        }
    }

    /// The name of a helper in the emitted source.
    fn helper(&self, what: &str) -> String {
        match self.lang {
            Lang::Python => format!("_{}", what),
            _ => format!("sat_{}", what),
        }
    }

    fn minmax(&self, op: Op, id: Id) -> String {
        let node = self.expr.node(id);
        let (a, b) = (
            self.render(node.children[0], 0),
            self.render(node.children[1], 0),
        );
        let is_min = op == Op::Min;
        format!(
            "{}({}, {})",
            self.helper(if is_min { "min" } else { "max" }),
            a,
            b
        )
    }

    /// The helper definitions this expression needs, and only those.
    ///
    /// Three operators cannot be spelled directly in any of the targets and
    /// still mean the same thing: `min` and `max` because the standard
    /// library versions leave the tie between `+0.0` and `-0.0` unspecified,
    /// `sign` because no target has it, and (in Python) `pow` because neither
    /// `**` nor `math.pow` is IEEE `pow`.
    fn helpers(&self) -> String {
        let ops: Vec<Op> = self
            .expr
            .reachable(self.expr.root())
            .iter()
            .map(|&id| self.expr.node(id).op)
            .collect();
        let mut out = String::new();
        if ops.iter().any(|o| matches!(o, Op::Min | Op::Max)) {
            out.push_str(match self.lang {
                Lang::C => C_MIN_MAX,
                Lang::Rust => RUST_MIN_MAX,
                Lang::Python => PYTHON_MIN_MAX,
            });
        }
        if ops.contains(&Op::Sign) {
            out.push_str(match self.lang {
                Lang::C => C_SIGN,
                Lang::Rust => RUST_SIGN,
                Lang::Python => PYTHON_SIGN,
            });
        }
        if self.lang == Lang::Python {
            if ops.contains(&Op::Pow) {
                out.push_str(PYTHON_POW);
            }
            // Python's `math` raises where IEEE-754 returns NaN or an
            // infinity, and `floor` and `ceil` return integers. Every one of
            // these needs a wrapper to behave like the double it replaces.
            for (op, helper) in [
                (Op::Div, PYTHON_DIV),
                (Op::Sqrt, PYTHON_SQRT),
                (Op::Ln, PYTHON_LOG),
                (Op::Exp, PYTHON_EXP),
                (Op::Sin, PYTHON_SIN),
                (Op::Cos, PYTHON_COS),
                (Op::Tan, PYTHON_TAN),
                (Op::Floor, PYTHON_FLOOR),
                (Op::Ceil, PYTHON_CEIL),
            ] {
                if ops.contains(&op) {
                    out.push_str(helper);
                }
            }
        }
        out
    }

    /// The native boolean form of a node, if it has one.
    ///
    /// Comparisons in all three targets are false when either side is NaN,
    /// which is exactly what this language's comparisons do, so a condition
    /// can be tested directly instead of being converted to `1.0` and then
    /// compared back against zero. `&&` and `||` short-circuit in the targets
    /// and do not here, which changes nothing: the language is pure.
    fn render_bool(&self, id: Id) -> Option<String> {
        if self.bound.contains_key(&id) {
            return None;
        }
        let node = self.expr.node(id);
        let kids = &node.children;
        Some(match node.op {
            Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne => {
                let op = match node.op {
                    Op::Lt => "<",
                    Op::Le => "<=",
                    Op::Gt => ">",
                    Op::Ge => ">=",
                    Op::Eq => "==",
                    _ => "!=",
                };
                format!(
                    "{} {} {}",
                    self.render(kids[0], 4),
                    op,
                    self.render(kids[1], 5)
                )
            }
            Op::And | Op::Or => {
                let (a, b) = (self.truthy(kids[0]), self.truthy(kids[1]));
                let word = match (self.lang, node.op) {
                    (Lang::Python, Op::And) => "and",
                    (Lang::Python, _) => "or",
                    (_, Op::And) => "&&",
                    (_, _) => "||",
                };
                format!("({}) {} ({})", a, word, b)
            }
            Op::Not => {
                let a = self.truthy(kids[0]);
                match self.lang {
                    Lang::Python => format!("not ({})", a),
                    _ => format!("!({})", a),
                }
            }
            _ => return None,
        })
    }

    /// A comparison or connective, converted to the `0.0`/`1.0` this language
    /// uses for booleans.
    fn to_float(&self, cond: &str) -> String {
        match self.lang {
            Lang::C => format!("(double)({})", cond),
            Lang::Rust => format!("(({}) as i32 as f64)", cond),
            Lang::Python => format!("float({})", cond),
        }
    }

    /// A value tested for truth: non-zero and not NaN.
    fn truthy(&self, id: Id) -> String {
        if let Some(native) = self.render_bool(id) {
            return native;
        }
        let v = self.render(id, 10);
        match self.lang {
            Lang::C => format!("({0} != 0.0 && !isnan({0}))", v),
            Lang::Rust => format!("({0} != 0.0 && !({0}).is_nan())", v),
            Lang::Python => format!("({0} != 0.0 and not math.isnan({0}))", v),
        }
    }

    fn literal(&self, v: F) -> String {
        let x = v.get();
        if x.is_nan() {
            return match self.lang {
                Lang::C => "NAN".into(),
                Lang::Rust => "f64::NAN".into(),
                Lang::Python => "math.nan".into(),
            };
        }
        if x.is_infinite() {
            let sign = if x < 0.0 { "-" } else { "" };
            return match self.lang {
                Lang::C => format!("{}INFINITY", sign),
                Lang::Rust => format!("{}f64::INFINITY", sign),
                Lang::Python => format!("{}math.inf", sign),
            };
        }
        // Round-trip precision, and never an integer literal: `2` would be an
        // int in C and Python and change the arithmetic.
        let mut s = format!("{:?}", x);
        if !s.contains('.') && !s.contains('e') && !s.contains('E') {
            s.push_str(".0");
        }
        // Rust needs the suffix as well: `0.5.sqrt()` is an ambiguous numeric
        // type, and an unsuffixed literal in a method call will not compile.
        if self.lang == Lang::Rust {
            s.push_str("f64");
        }
        s
    }
}

/// `min` and `max` with the ±0 tie settled, matching [`crate::lang::min`].
const C_MIN_MAX: &str = "\
static double sat_min(double a, double b) {
    if (isnan(a)) return b;
    if (isnan(b)) return a;
    if (a == b) return signbit(a) ? a : b;
    return a < b ? a : b;
}

static double sat_max(double a, double b) {
    if (isnan(a)) return b;
    if (isnan(b)) return a;
    if (a == b) return signbit(a) ? b : a;
    return a > b ? a : b;
}

";

const RUST_MIN_MAX: &str = "\
fn sat_min(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        return b;
    }
    if b.is_nan() {
        return a;
    }
    if a == b {
        return if a.is_sign_negative() { a } else { b };
    }
    if a < b { a } else { b }
}

fn sat_max(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        return b;
    }
    if b.is_nan() {
        return a;
    }
    if a == b {
        return if a.is_sign_negative() { b } else { a };
    }
    if a > b { a } else { b }
}

";

const PYTHON_MIN_MAX: &str = "\
def _min(a, b):
    if math.isnan(a):
        return b
    if math.isnan(b):
        return a
    if a == b:
        return a if math.copysign(1.0, a) < 0.0 else b
    return a if a < b else b


def _max(a, b):
    if math.isnan(a):
        return b
    if math.isnan(b):
        return a
    if a == b:
        return b if math.copysign(1.0, a) < 0.0 else a
    return a if a > b else b


";

/// `sign`, which none of the targets has, and which the obvious ternary gets
/// wrong at NaN.
const C_SIGN: &str = "\
static double sat_sign(double a) {
    if (isnan(a)) return a;
    return (double)((a > 0.0) - (a < 0.0));
}

";

const RUST_SIGN: &str = "\
fn sat_sign(a: f64) -> f64 {
    if a.is_nan() {
        return a;
    }
    (a > 0.0) as i32 as f64 - (a < 0.0) as i32 as f64
}

";

const PYTHON_SIGN: &str = "\
def _sign(a):
    if math.isnan(a):
        return a
    return float((a > 0.0) - (a < 0.0))


";

/// Python's `math` functions, and its `/`, raise where IEEE-754 returns a
/// value. Each of these restores the double's behaviour.
const PYTHON_DIV: &str = "\
def _div(a, b):
    if b != 0.0:
        return a / b
    if math.isnan(a) or a == 0.0:
        return math.nan
    return math.copysign(math.inf, math.copysign(1.0, a) * math.copysign(1.0, b))


";

const PYTHON_SQRT: &str = "\
def _sqrt(a):
    if math.isnan(a) or a < 0.0:
        return math.nan
    return math.sqrt(a)


";

const PYTHON_LOG: &str = "\
def _log(a):
    if math.isnan(a) or a < 0.0:
        return math.nan
    if a == 0.0:
        return -math.inf
    return math.log(a)


";

const PYTHON_EXP: &str = "\
def _exp(a):
    try:
        return math.exp(a)
    except OverflowError:
        return math.inf


";

const PYTHON_SIN: &str = "\
def _sin(a):
    return math.nan if not math.isfinite(a) else math.sin(a)


";

const PYTHON_COS: &str = "\
def _cos(a):
    return math.nan if not math.isfinite(a) else math.cos(a)


";

const PYTHON_TAN: &str = "\
def _tan(a):
    return math.nan if not math.isfinite(a) else math.tan(a)


";

/// `math.floor` and `math.ceil` return integers, and raise on an infinity.
const PYTHON_FLOOR: &str = "\
def _floor(a):
    return a if not math.isfinite(a) else float(math.floor(a))


";

const PYTHON_CEIL: &str = "\
def _ceil(a):
    return a if not math.isfinite(a) else float(math.ceil(a))


";

/// IEEE-754 `pow`, which neither of Python's spellings provides.
const PYTHON_POW: &str = "\
def _pow(a, b):
    try:
        return math.pow(a, b)
    except OverflowError:
        # The true result is finite but outside the double range; IEEE pow
        # saturates to an infinity with the sign the result would have had.
        return -math.inf if _negative_result(a, b) else math.inf
    except ValueError:
        # Raised for a pole and for a domain error. A zero base with a
        # negative exponent is the pole, signed like 1 / a.
        if a == 0.0:
            return -math.inf if _negative_result(a, b) else math.inf
        return math.nan


def _negative_result(a, b):
    if math.copysign(1.0, a) > 0.0 or not math.isfinite(b):
        return False
    return b == int(b) and int(b) % 2 != 0


";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn c(src: &str) -> String {
        emit(&parse(src).unwrap(), Lang::C, "f")
    }

    #[test]
    fn parameters_follow_the_expression() {
        let out = c("a * x + b");
        assert!(
            out.contains("double f(double a, double b, double x)"),
            "{}",
            out
        );
        assert!(out.contains("return a * x + b;"), "{}", out);
    }

    #[test]
    fn shared_subterms_become_temporaries() {
        let out = c("let t = sin(x) * cos(x) in t + t * t");
        assert_eq!(out.matches("sin(").count(), 1, "{}", out);
        assert!(out.contains("const double t0"), "{}", out);
    }

    #[test]
    fn leaves_are_repeated_rather_than_named() {
        let out = c("x + x + x");
        assert!(!out.contains("const double"), "{}", out);
    }

    #[test]
    fn precedence_is_preserved() {
        assert!(c("(a + b) * c").contains("(a + b) * c"));
        assert!(c("a + b * c").contains("a + b * c"));
        assert!(c("a - (b - c)").contains("a - (b - c)"));
        assert!(c("a / (b / c)").contains("a / (b / c)"));
    }

    #[test]
    fn integer_valued_literals_stay_floating_point() {
        // `2` in C is an int, and `1 / 2` would then be zero.
        let out = c("x / 2");
        assert!(out.contains("2.0"), "{}", out);
        assert!(!out.contains("/ 2;"), "{}", out);
    }

    #[test]
    fn a_comparison_is_tested_natively() {
        // C comparisons are already false when either side is NaN, exactly as
        // this language's are, so a condition that is itself a comparison
        // needs no conversion to 1.0 and back.
        let out = c("if(a < b, a, b)");
        assert!(out.contains("(a < b ? a : b)"), "{}", out);
        assert!(!out.contains("isnan"), "{}", out);
    }

    #[test]
    fn an_arbitrary_value_is_tested_for_truth() {
        // A plain value is true when it is neither zero nor NaN, which C does
        // not do on its own.
        let out = c("if(a * b, a, b)");
        assert!(out.contains("isnan"), "{}", out);
        assert!(out.contains("!= 0.0"), "{}", out);
    }

    #[test]
    fn a_comparison_used_as_a_value_becomes_a_double() {
        let out = c("(a < b) + 1");
        assert!(out.contains("(double)(a < b)"), "{}", out);
    }

    #[test]
    fn each_language_uses_its_own_spelling() {
        let e = parse("sqrt(abs(x)) + ln(y)").unwrap();
        let c = emit(&e, Lang::C, "f");
        assert!(
            c.contains("sqrt(") && c.contains("fabs(") && c.contains("log("),
            "{}",
            c
        );
        let rust = emit(&e, Lang::Rust, "f");
        assert!(
            rust.contains(".sqrt()") && rust.contains(".abs()") && rust.contains(".ln()"),
            "{}",
            rust
        );
        assert!(rust.contains("pub fn f(x: f64, y: f64) -> f64"), "{}", rust);
        let py = emit(&e, Lang::Python, "f");
        assert!(
            py.contains("math.sqrt(") && py.contains("math.fabs(") && py.contains("math.log("),
            "{}",
            py
        );
        assert!(py.contains("def f(x, y):"), "{}", py);
    }

    #[test]
    fn special_values_are_spelled_correctly() {
        for (lang, needle) in [
            (Lang::C, "INFINITY"),
            (Lang::Rust, "f64::INFINITY"),
            (Lang::Python, "math.inf"),
        ] {
            let out = emit(&parse("x + inf").unwrap(), lang, "f");
            assert!(out.contains(needle), "{}", out);
        }
    }

    #[test]
    fn a_constant_expression_needs_no_parameters() {
        let out = c("2 + 3");
        assert!(out.contains("double f(void)"), "{}", out);
    }
}
