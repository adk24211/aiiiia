//! Differential testing of two expressions.
//!
//! CONTRACT ONLY — the bodies are unimplemented. The public API below is
//! fixed; the implementation fills it in.

use crate::lang::RecExpr;
use crate::sym::Sym;

/// One disagreeing input.
#[derive(Clone, Debug)]
pub struct Sample {
    pub bindings: Vec<(Sym, f64)>,
    pub left: f64,
    pub right: f64,
    pub rel_err: f64,
}

/// The outcome of comparing two expressions over many random inputs.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub samples: usize,
    pub agreed: usize,
    pub disagreed: usize,
    pub both_nan: usize,
    pub max_rel_err: f64,
    pub worst: Option<Sample>,
    pub tolerance: f64,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.disagreed == 0
    }
    pub fn render(&self) -> String {
        unimplemented!()
    }
}

/// Configures a comparison run.
#[derive(Clone, Debug)]
pub struct Checker {
    pub samples: usize,
    pub seed: u64,
    pub tolerance: f64,
    /// Include infinities, subnormals, and huge magnitudes in the inputs.
    pub wild: bool,
}

impl Default for Checker {
    fn default() -> Checker {
        Checker {
            samples: 2_000,
            seed: 0x5A7,
            tolerance: 1e-9,
            wild: false,
        }
    }
}

impl Checker {
    pub fn new() -> Checker {
        Checker::default()
    }
    pub fn with_samples(mut self, n: usize) -> Self {
        self.samples = n;
        self
    }
    pub fn with_seed(mut self, s: u64) -> Self {
        self.seed = s;
        self
    }
    pub fn with_tolerance(mut self, t: f64) -> Self {
        self.tolerance = t;
        self
    }
    pub fn with_wild(mut self, w: bool) -> Self {
        self.wild = w;
        self
    }
    pub fn compare(&self, _a: &RecExpr, _b: &RecExpr) -> Report {
        unimplemented!()
    }
}
