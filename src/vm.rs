//! Bytecode compiler and register machine.
//!
//! The extracted expression is already a topologically sorted, maximally
//! shared DAG, so every node is computed exactly once and common-subexpression
//! elimination costs nothing — it is a property of the representation rather
//! than a pass. What the compiler adds is constant folding, slot recycling,
//! and a linear instruction stream.

use crate::eval::{Env, EvalError};
use crate::lang::{Id, Op, RecExpr};
use crate::sym::Sym;
use std::collections::HashMap;
use std::fmt::Write as _;

/// One instruction. `dst` and the operand fields index the slot array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instr {
    LoadConst { dst: u16, k: u16 },
    LoadParam { dst: u16, p: u16 },
    Un { dst: u16, op: Op, a: u16 },
    Bin { dst: u16, op: Op, a: u16, b: u16 },
    Select { dst: u16, c: u16, a: u16, b: u16 },
}

impl Instr {
    pub fn dst(&self) -> u16 {
        match *self {
            Instr::LoadConst { dst, .. }
            | Instr::LoadParam { dst, .. }
            | Instr::Un { dst, .. }
            | Instr::Bin { dst, .. }
            | Instr::Select { dst, .. } => dst,
        }
    }
}

/// A compiled expression.
#[derive(Clone, Debug)]
pub struct Program {
    pub code: Vec<Instr>,
    pub consts: Vec<f64>,
    /// Variables the program reads, sorted by name. `eval` takes its arguments
    /// in this order.
    pub params: Vec<Sym>,
    /// Size of the slot array `eval` needs.
    pub slots: usize,
}

impl Program {
    /// Compile `expr`, folding anything whose operands are all literals.
    pub fn compile(expr: &RecExpr) -> Result<Program, EvalError> {
        Compiler::new(expr).run()
    }

    pub fn params(&self) -> &[Sym] {
        &self.params
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }

    /// Run the program. `args` must be in [`Program::params`] order.
    ///
    /// One allocation per call for the slot array. Callers evaluating the same
    /// program many times should use [`Program::eval_with`] and keep the
    /// buffer.
    ///
    /// # Panics
    ///
    /// If `args.len()` is not [`Program::params`]`.len()`. The fields of
    /// `Program` are public, so a hand-built program that names a slot beyond
    /// `slots`, a constant past the pool, or a parameter past `params` will
    /// also panic on the offending index. Programs from
    /// [`Program::compile`] never do.
    pub fn eval(&self, args: &[f64]) -> f64 {
        let mut slots = vec![0.0; self.slots];
        self.eval_with(args, &mut slots)
    }

    /// Run the program using a caller-owned slot array, which is resized if it
    /// is too small.
    pub fn eval_with(&self, args: &[f64], slots: &mut Vec<f64>) -> f64 {
        assert_eq!(
            args.len(),
            self.params.len(),
            "this program takes {} argument{} ({}), not {}",
            self.params.len(),
            if self.params.len() == 1 { "" } else { "s" },
            self.params
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            args.len()
        );
        if slots.len() < self.slots {
            slots.resize(self.slots, 0.0);
        }
        let s = &mut slots[..self.slots];
        let truthy = |x: f64| x != 0.0 && !x.is_nan();
        let mut last = 0.0;
        for instr in &self.code {
            let v = match *instr {
                Instr::LoadConst { k, .. } => self.consts[k as usize],
                Instr::LoadParam { p, .. } => args[p as usize],
                Instr::Un { op, a, .. } => apply1(op, s[a as usize]),
                Instr::Bin { op, a, b, .. } => apply2(op, s[a as usize], s[b as usize]),
                Instr::Select { c, a, b, .. } => {
                    if truthy(s[c as usize]) {
                        s[a as usize]
                    } else {
                        s[b as usize]
                    }
                }
            };
            s[instr.dst() as usize] = v;
            last = v;
        }
        last
    }

    /// Run the program with variables looked up by name.
    pub fn eval_env(&self, env: &Env) -> Result<f64, EvalError> {
        let mut args = Vec::with_capacity(self.params.len());
        for p in &self.params {
            args.push(*env.get(p).ok_or(EvalError::Unbound(*p))?);
        }
        Ok(self.eval(&args))
    }

    /// Evaluate the program once per row, reusing one slot array.
    pub fn eval_batch(&self, rows: &[&[f64]], out: &mut Vec<f64>) {
        let mut slots = vec![0.0; self.slots];
        out.clear();
        out.reserve(rows.len());
        for row in rows {
            out.push(self.eval_with(row, &mut slots));
        }
    }

    /// A readable listing of the program.
    pub fn disassemble(&self) -> String {
        let mut s = String::new();
        let params = if self.params.is_empty() {
            "(none)".to_string()
        } else {
            self.params
                .iter()
                .enumerate()
                .map(|(i, p)| format!("p{}={}", i, p))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = writeln!(s, "  params    {}", params);
        let consts = if self.consts.is_empty() {
            "(none)".to_string()
        } else {
            self.consts
                .iter()
                .enumerate()
                .map(|(i, c)| format!("k{}={}", i, crate::sym::F::new(*c)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = writeln!(s, "  consts    {}", consts);
        let _ = writeln!(
            s,
            "  {} instruction{}, {} slot{}",
            self.code.len(),
            if self.code.len() == 1 { "" } else { "s" },
            self.slots,
            if self.slots == 1 { "" } else { "s" }
        );
        s.push('\n');
        for (i, instr) in self.code.iter().enumerate() {
            let body = match *instr {
                Instr::LoadConst { dst, k } => format!(
                    "s{:<3} = k{}            ; {}",
                    dst,
                    k,
                    crate::sym::F::new(self.consts[k as usize])
                ),
                Instr::LoadParam { dst, p } => {
                    format!(
                        "s{:<3} = p{}            ; {}",
                        dst, p, self.params[p as usize]
                    )
                }
                Instr::Un { dst, op, a } => {
                    format!("s{:<3} = {} s{}", dst, pad(op.name()), a)
                }
                Instr::Bin { dst, op, a, b } => {
                    format!("s{:<3} = {} s{}, s{}", dst, pad(op.name()), a, b)
                }
                Instr::Select { dst, c, a, b } => {
                    format!("s{:<3} = {} s{}, s{}, s{}", dst, pad("select"), c, a, b)
                }
            };
            let _ = writeln!(s, "  {:>4}  {}", i, body);
        }
        if let Some(last) = self.code.last() {
            let _ = writeln!(s, "\n  result in s{}", last.dst());
        }
        s
    }
}

fn pad(name: &str) -> String {
    format!("{:<6}", name)
}

#[inline]
fn apply1(op: Op, a: f64) -> f64 {
    use Op::*;
    match op {
        Neg => -a,
        Sqrt => a.sqrt(),
        Ln => a.ln(),
        Exp => a.exp(),
        Sin => a.sin(),
        Cos => a.cos(),
        Tan => a.tan(),
        Abs => a.abs(),
        Floor => a.floor(),
        Ceil => a.ceil(),
        // Everything else is either not unary or has no fast path worth
        // duplicating; fall back to the shared definition so the VM and the
        // interpreter can never drift apart.
        other => other.eval(&[a]).expect("unary operator"),
    }
}

#[inline]
fn apply2(op: Op, a: f64, b: f64) -> f64 {
    use Op::*;
    match op {
        Add => a + b,
        Sub => a - b,
        Mul => a * b,
        Div => a / b,
        Pow => a.powf(b),
        // Not `f64::min`/`max`: those leave the ±0 tie unspecified, and the
        // VM has to agree with the interpreter bit for bit.
        Min => crate::lang::min(a, b),
        Max => crate::lang::max(a, b),
        other => other.eval(&[a, b]).expect("binary operator"),
    }
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

/// The compile-time value of `node`, if every operand already has one.
fn fold(node: &crate::lang::ENode, known: &HashMap<Id, f64>) -> Option<f64> {
    if let Op::Const(c) = node.op {
        return Some(c.get());
    }
    if !node.op.is_foldable() {
        return None;
    }
    let mut args = Vec::with_capacity(node.children().len());
    for &c in node.children() {
        args.push(*known.get(&c)?);
    }
    node.op.eval(&args)
}

struct Compiler<'a> {
    expr: &'a RecExpr,
    order: Vec<Id>,
    code: Vec<Instr>,
    consts: Vec<f64>,
    const_index: HashMap<u64, u16>,
    params: Vec<Sym>,
    /// Slot holding each already-emitted node, or `None` if the node folded.
    slot_of: HashMap<Id, u16>,
    /// Compile-time value of each node, when it has one.
    value_of: HashMap<Id, f64>,
    free: Vec<u16>,
    high_water: usize,
}

impl<'a> Compiler<'a> {
    fn new(expr: &'a RecExpr) -> Compiler<'a> {
        let order = expr.reachable(expr.root());
        let mut params: Vec<Sym> = order
            .iter()
            .filter_map(|&id| expr.node(id).as_var())
            .collect();
        params.sort_by_key(|s| s.as_str());
        params.dedup();
        Compiler {
            expr,
            order,
            code: Vec::new(),
            consts: Vec::new(),
            const_index: HashMap::new(),
            params,
            slot_of: HashMap::new(),
            value_of: HashMap::new(),
            free: Vec::new(),
            high_water: 0,
        }
    }

    fn intern_const(&mut self, x: f64) -> Result<u16, EvalError> {
        let key = if x.is_nan() {
            0x7ff8_0000_0000_0000
        } else {
            x.to_bits()
        };
        if let Some(&i) = self.const_index.get(&key) {
            return Ok(i);
        }
        let i = u16::try_from(self.consts.len())
            .map_err(|_| EvalError::ProgramTooLarge("distinct constants"))?;
        self.consts.push(x);
        self.const_index.insert(key, i);
        Ok(i)
    }

    fn alloc(&mut self) -> Result<u16, EvalError> {
        match self.free.pop() {
            Some(s) => Ok(s),
            None => {
                let s = u16::try_from(self.high_water)
                    .map_err(|_| EvalError::ProgramTooLarge("live values"))?;
                self.high_water += 1;
                Ok(s)
            }
        }
    }

    fn run(mut self) -> Result<Program, EvalError> {
        let root = self.expr.root();

        // Pass one: fold. A node has a compile-time value when every operand
        // does, so this single forward pass over the topological order settles
        // the whole expression.
        for &id in &self.order {
            let node = self.expr.node(id);
            if node.op == Op::Diff {
                return Err(EvalError::UnreducedDiff);
            }
            if let Some(v) = fold(node, &self.value_of) {
                self.value_of.insert(id, v);
            }
        }

        // A folded node is only worth materializing when something that is
        // *not* folded reads it. Otherwise the whole constant subtree
        // disappears into its parent's literal.
        let mut emitted: Vec<Id> = Vec::with_capacity(self.order.len());
        let mut needed: std::collections::HashSet<Id> = std::collections::HashSet::new();
        needed.insert(root);
        for &id in &self.order {
            // A folded node emits a single literal and never reads its
            // operands, so it keeps nothing below it alive -- not even when it
            // is the root.
            if self.value_of.contains_key(&id) {
                continue;
            }
            for &c in self.expr.node(id).children() {
                needed.insert(c);
            }
        }
        for &id in &self.order {
            let folded = self.value_of.contains_key(&id);
            if !folded || needed.contains(&id) {
                emitted.push(id);
            }
        }

        // Last use, over the emitted nodes only: a slot is free again once the
        // instruction at that position has run.
        let position: HashMap<Id, usize> =
            emitted.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let mut last_use: HashMap<Id, usize> = HashMap::new();
        for (i, &id) in emitted.iter().enumerate() {
            if self.value_of.contains_key(&id) {
                continue;
            }
            for &c in self.expr.node(id).children() {
                if position.contains_key(&c) {
                    last_use.insert(c, i);
                }
            }
        }

        // Pass two: emit.
        for (i, id) in emitted.iter().copied().enumerate() {
            let node = *self.expr.node(id);
            let dst = match self.value_of.get(&id).copied() {
                Some(v) => {
                    let k = self.intern_const(v)?;
                    let dst = self.alloc()?;
                    self.code.push(Instr::LoadConst { dst, k });
                    dst
                }
                None => self.emit(&node, id)?,
            };
            self.slot_of.insert(id, dst);

            // Recycle the slots of operands whose last use was this
            // instruction. Deduplicate first: `x * x` names the same child
            // twice, and freeing its slot twice would hand it to two different
            // nodes at once.
            let mut dead: Vec<Id> = node
                .children()
                .iter()
                .copied()
                .filter(|c| last_use.get(c) == Some(&i) && *c != root)
                .collect();
            dead.sort();
            dead.dedup();
            for c in dead {
                if let Some(s) = self.slot_of.get(&c).copied() {
                    if s != dst {
                        self.free.push(s);
                    }
                }
            }
        }

        let slots = self.high_water.max(1);
        Ok(Program {
            code: self.code,
            consts: self.consts,
            params: self.params,
            slots,
        })
    }

    fn emit(&mut self, node: &crate::lang::ENode, _id: Id) -> Result<u16, EvalError> {
        // Operand slots are read before the destination is written, so it is
        // safe for the destination to reuse one of them -- but allocation
        // happens after the reads are recorded, and freeing is deferred to the
        // caller, so this cannot happen here anyway.
        let operands: Vec<u16> = node
            .children()
            .iter()
            .map(|c| {
                self.slot_of
                    .get(c)
                    .copied()
                    .expect("children are compiled before their parent")
            })
            .collect();
        let dst = self.alloc()?;
        let instr = match node.op {
            Op::Var(s) => {
                let p = self
                    .params
                    .iter()
                    .position(|q| *q == s)
                    .expect("every variable is a parameter") as u16;
                Instr::LoadParam { dst, p }
            }
            Op::Const(c) => {
                let k = self.intern_const(c.get())?;
                Instr::LoadConst { dst, k }
            }
            Op::If => Instr::Select {
                dst,
                c: operands[0],
                a: operands[1],
                b: operands[2],
            },
            op if op.arity() == 1 => Instr::Un {
                dst,
                op,
                a: operands[0],
            },
            op if op.arity() == 2 => Instr::Bin {
                dst,
                op,
                a: operands[0],
                b: operands[1],
            },
            // `Select` runs `If` unconditionally, so it must not become the
            // silent home of any other three-argument operator added later.
            op => unreachable!(
                "{:?} takes {} children and has no instruction",
                op,
                op.arity()
            ),
        };
        self.code.push(instr);
        Ok(dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::eval;
    use crate::gen::{ExprStream, Grammar};
    use crate::parser::parse;
    use crate::rng::Rng;

    fn prog(src: &str) -> Program {
        Program::compile(&parse(src).unwrap()).unwrap()
    }

    #[test]
    fn constants_fold_at_compile_time() {
        let p = prog("2 * 3 + 4 * 5");
        assert_eq!(p.code.len(), 1, "{}", p.disassemble());
        assert_eq!(p.eval(&[]), 26.0);
        assert!(p.params.is_empty());
    }

    #[test]
    fn a_constant_subtree_costs_one_instruction() {
        let p = prog("x + (2 * 3 + 4 * 5)");
        // One load for x, one for the folded 26, one add.
        assert_eq!(p.code.len(), 3, "{}", p.disassemble());
    }

    #[test]
    fn parameters_are_sorted_by_name() {
        let p = prog("z * a + m");
        let names: Vec<String> = p.params.iter().map(|s| s.to_string()).collect();
        assert_eq!(names, vec!["a", "m", "z"]);
        // a=2, m=5, z=3  ->  3*2 + 5
        assert_eq!(p.eval(&[2.0, 5.0, 3.0]), 11.0);
    }

    #[test]
    fn sharing_is_computed_once() {
        let p = prog("let t = sin(x) * cos(x) in t + t * t");
        let sines = p
            .code
            .iter()
            .filter(|i| matches!(i, Instr::Un { op: Op::Sin, .. }))
            .count();
        assert_eq!(sines, 1, "{}", p.disassemble());
    }

    #[test]
    fn slots_are_recycled() {
        // A left-leaning chain only ever has two values live at a time.
        let src = (0..60)
            .map(|i| format!("x + {}", i))
            .collect::<Vec<_>>()
            .join(" + ");
        let p = Program::compile(&parse(&src).unwrap()).unwrap();
        assert!(
            p.slots <= 4,
            "a linear chain needed {} slots:\n{}",
            p.slots,
            p.disassemble()
        );
    }

    #[test]
    fn a_derivative_node_is_rejected() {
        let e = parse("d(x, x * x)").unwrap();
        assert_eq!(
            Program::compile(&e).err(),
            Some(EvalError::UnreducedDiff),
            "a derivative node must not compile"
        );
    }

    #[test]
    fn eval_env_reports_an_unbound_variable() {
        let p = prog("x + y");
        let env: Env = [(Sym::new("x"), 1.0)].into_iter().collect();
        assert_eq!(
            p.eval_env(&env).err(),
            Some(EvalError::Unbound(Sym::new("y")))
        );
    }

    #[test]
    #[should_panic(expected = "takes 2 arguments")]
    fn the_wrong_number_of_arguments_is_rejected() {
        prog("x + y").eval(&[1.0]);
    }

    #[test]
    fn conditionals_select_without_evaluating_both_branches_twice() {
        let p = prog("if(x < 0, -x, x)");
        assert_eq!(p.eval(&[-3.0]), 3.0);
        assert_eq!(p.eval(&[3.0]), 3.0);
    }

    #[test]
    fn the_vm_reproduces_the_interpreter_bit_for_bit() {
        let mut rng = Rng::seed(0x5B17);
        for expr in ExprStream::new(3, Grammar::default(), 5).take(500) {
            let p = Program::compile(&expr).unwrap();
            for _ in 0..8 {
                let args: Vec<f64> = p.params.iter().map(|_| rng.float()).collect();
                let env: Env = p.params.iter().copied().zip(args.iter().copied()).collect();
                let want = eval(&expr, &env).unwrap();
                let got = p.eval(&args);
                assert!(
                    want.to_bits() == got.to_bits() || (want.is_nan() && got.is_nan()),
                    "{}\n  args {:?}\n  interpreter {:?}\n  vm          {:?}\n{}",
                    expr.pretty(),
                    args,
                    want,
                    got,
                    p.disassemble()
                );
            }
        }
    }

    #[test]
    fn batch_evaluation_matches_single() {
        let p = prog("sqrt(x * x + y * y)");
        let rows: Vec<Vec<f64>> = (0..50)
            .map(|i| vec![i as f64, (i * 3 % 7) as f64])
            .collect();
        let refs: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        let mut out = Vec::new();
        p.eval_batch(&refs, &mut out);
        for (row, got) in rows.iter().zip(&out) {
            assert_eq!(p.eval(row), *got);
        }
    }

    #[test]
    fn disassembly_names_every_parameter() {
        let p = prog("a * b + c / d");
        let text = p.disassemble();
        for name in ["a", "b", "c", "d"] {
            assert!(text.contains(name), "{} missing from\n{}", name, text);
        }
        assert!(text.contains("result in s"));
    }
}
