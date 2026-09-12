//! Bytecode compiler and virtual machine.
//!
//! CONTRACT ONLY — the bodies are unimplemented. The public API below is
//! fixed; the implementation fills it in.

use crate::eval::{Env, EvalError};
use crate::lang::RecExpr;
use crate::sym::Sym;

/// A compiled expression.
#[derive(Clone, Debug)]
pub struct Program {
    pub code: Vec<Instr>,
    pub consts: Vec<f64>,
    pub params: Vec<Sym>,
    pub slots: usize,
}

/// One instruction. `dst` and `src` index the slot array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instr {
    LoadConst { dst: u16, k: u16 },
    LoadParam { dst: u16, p: u16 },
    Un { dst: u16, op: crate::lang::Op, a: u16 },
    Bin { dst: u16, op: crate::lang::Op, a: u16, b: u16 },
    Select { dst: u16, c: u16, a: u16, b: u16 },
}

impl Program {
    pub fn compile(_expr: &RecExpr) -> Result<Program, EvalError> {
        unimplemented!()
    }
    pub fn eval(&self, _args: &[f64]) -> f64 {
        unimplemented!()
    }
    pub fn eval_env(&self, _env: &Env) -> Result<f64, EvalError> {
        unimplemented!()
    }
    pub fn disassemble(&self) -> String {
        unimplemented!()
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
}
