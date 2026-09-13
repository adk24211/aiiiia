# Working on saturn

An equality saturation engine for a small numeric language. `docs/design.md`
explains how it works and why; read it before changing the core.

Everything below is about saturn: the crate at the repository root. The
separate project in `hookline/` is a network service with its own crate,
dependencies, tests and conventions — see `hookline/CLAUDE.md`.

## Ground rules

**Zero dependencies.** The crate builds on `std` alone, and it stays that way.
If something seems to need a crate, it is usually forty lines (`src/rng.rs`,
`src/fxhash.rs`) and worth writing.

**A rule is a claim, and claims are tested.** Every rule in the safe tier is
verified in isolation by `tests/differential.rs` — unconditional ones against
their own right-hand side over hostile inputs at tolerance zero, conditional
ones by building the left side in a real e-graph and asking the analysis the
same question the rule asks. Adding a safe rule that is not exact will fail
that test by name. That is the point; do not weaken it.

**`safe` means bit-for-bit.** A rule belongs in `safe()` only if it reproduces
the IEEE-754 result for *every* input, infinities and NaN and both zeros
included, or if a side condition rules out the cases where it would not.
Everything else goes in `fast_math()`. When you are not certain, it is
fast-math.

**Commutativity is structural.** Commutative e-nodes store their children in a
canonical order and the matcher tries both. Never write a commutativity rule,
and never write a rule that is only the mirror image of another.

**Constant folding is automatic.** The analysis folds any node whose children
are all known finite constants. Never write a rule that folds constants.

## Before you commit

```
cargo fmt
cargo clippy --all-targets      # must be silent
cargo test --release            # must be green
cargo run --release --bin saturn -- fuzz --rules safe --count 2000
```

The fuzzer is not optional when you touch a rule, the analysis, or the
extractor. CI runs it too.

## Things that have bitten

These are all in the git history with the test that caught them. They are here
because they are the kind of thing that comes back.

* **Signed zero is a value.** `-0.0` and `0.0` compare equal but `1 / -0.0` is
  `-inf`. They are distinct literals, distinct e-nodes, distinct classes. A
  `0` in a pattern matches only `+0.0`.
* **`f64::min` and `f64::max` are not commutative.** They return "either
  input" when the two compare equal, so on `±0` the answer depends on operand
  order. Use `lang::min` and `lang::max`, which settle it.
* **An analysis step that can move *up* its lattice never converges.**
  `Interval::meet` jumps to TOP on a contradiction for exactly this reason.
* **A limit checked between iterations is not a limit.** A single rule's
  e-matching can outlast the whole budget. The clock is read between rules and
  during application, and the matcher has a step budget — capping its *results*
  does not bound its *work*.
* **Extraction is optimal for tree cost, not DAG cost.** It can return an
  expression that is larger once sharing is counted. `optimize` keeps the
  input when that happens.

## Writing

Comments explain why, never what. If a line needs a comment to say what it
does, rewrite the line. Every public item has rustdoc. No emoji. Nothing in
the repository refers to how it was written.
