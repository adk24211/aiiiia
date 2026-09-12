# saturn

**Equality saturation for a small numeric language.** An optimizer that does
not have to guess which rewrite to apply, because it applies all of them.

Zero dependencies, Rust 1.86+, library and CLI. ~11k lines, 228 tests.

```
cargo run -- opt 'a*x^3 + b*x^2 + c*x + d' --rules all
cargo run -- diff x 'exp(sin(x * x))'
cargo run -- emit 'u / w + v / w' --rules all --lang rust
cargo run -- why 'x*y + x*z' 'x*(y + z)' --rules all
cargo run -- opt 'w / w * x' --assume 'finite(w) && nonzero(w)'
cargo run -- fuzz --rules safe --count 5000
```

---

## The idea

A conventional optimizer holds one program and rewrites it in place, which
means it must decide at every step which rule to apply — and the decision is
destructive. Distribute `a * (b + c)` and you can no longer cancel a common
factor; cancel first and you may miss a better factoring downstream. This is
the phase-ordering problem, and pass pipelines are a way of guessing well
rather than of not having to guess.

Equality saturation removes the guess. A rewrite does not *replace* a term; it
records that two terms are **equal**, and keeps both. Do that until the rules
stop finding anything new and you hold a structure — an **e-graph** —
containing every program the rules can reach, exponentially many of them in
polynomial space. Only then do you pick one, with a cost model and full
knowledge of the alternatives.

`saturn` implements that from first principles: an e-graph with congruence
closure and deferred rebuilding, e-matching, an interval-based e-class
analysis that discharges the side conditions float rewriting needs, a backoff
rule scheduler, cost extraction, derivations that justify an equality step by
step, a symbolic differentiator written entirely as rewrite rules, a bytecode
compiler, and a code emitter whose output is compiled and checked against the
interpreter bit for bit.

`docs/design.md` is the full tour.

---

## What it does

Here is a polynomial and 201 rules, none of which mentions Horner's rule:

<!-- DEMO:opt -->

```console
$ saturn opt 'a*x^3 + b*x^2 + c*x + d' --rules all --stats
  input     a * x ^ 3 + b * x ^ 2 + x * c + d
            15 nodes, 175.875 ops

  optimized x * (c + x * (b + x * a)) + d
            11 nodes, 15.625 ops  91% cheaper

  e-graph   32 classes, 61 nodes, 6 iterations, 1.0ms (saturated)
  rules     211 rules from `all`

  stopped: saturated
  iterations: 6
  classes: 32
  nodes: 61
  total time: 1.05ms
  rules that fired:
        20  assoc-add
         8  factor
         6  assoc-mul
         2  expand-pow
         1  mul-pow

  iteration    classes    nodes   matches   time
          0         19       25         7   104.9µs
          1         29       47        33   118.8µs
          2         31       56        89   188.5µs
          3         34       63       123   202.9µs
          4         32       61       141   237.6µs
          5         32       61       141   195.8µs
```

Every rule that fired is a one-line local identity. Horner's form is what falls
out of keeping all of them and letting a cost model choose at the end; a
conventional pass pipeline has to be *told* to look for it.

A smaller one, for the mechanism:

<!-- DEMO:divide -->

```console
$ saturn opt 'u / w + v / w' --rules all
  input     u / w + v / w
            6 nodes, 31.375 ops

  optimized (u + v) / w
            5 nodes, 16.375 ops  48% cheaper

  e-graph   7 classes, 8 nodes, 2 iterations, 129.2µs (saturated)
  rules     211 rules from `all`
```

Nothing in the rule library knows about that expression either. The engine
found it because the e-graph held `u / w + v / w` and `(u + v) / w` at the same
time, and the cost model prices a divide at fifteen adds.

### It shows its work

An optimizer that says "these are the same" and cannot say why is asking to be
trusted. Every union carries a reason, and `--why` walks the chain back into a
derivation:

<!-- DEMO:why -->

```console
$ saturn opt 'a*x^3 + b*x^2 + c*x + d' --rules all --why
  input     a * x ^ 3 + b * x ^ 2 + x * c + d
            15 nodes, 175.875 ops

  optimized x * (c + x * (b + x * a)) + d
            11 nodes, 15.625 ops  91% cheaper

  e-graph   32 classes, 61 nodes, 6 iterations, 1.1ms (saturated)
  rules     211 rules from `all`

  yes in 4 steps
  using congruence x3, assoc-mul

  1. congruence   both sides are `+` applied to equal arguments, and
        argument 2:
          1. congruence   both sides are `+` applied to equal arguments, and
                argument 1:
                  1. congruence   both sides are `*` applied to equal arguments, and
                        argument 2:
                          1. assoc-mul   ?a * ?b * ?c => ?a * (?b * ?c)
                                ?a = a
                                ?b = x
                                ?c = x
```

Congruence steps unfold, because "the same operator over equivalent arguments"
is not an explanation until the arguments are explained too. `saturn why a b`
does the same for any two expressions, and exits non-zero when the rules
cannot prove them equal — saying whether that is a real negative (the rules
saturated) or just a budget that ran out.

Recording is off by default and observation-only: a test asserts that a run
with explanations enabled produces the same graph and the same result as one
without, since otherwise the derivation would describe a different run than
the one it explains.

### Differentiation is just more rules

<!-- DEMO:diff -->

```console
$ saturn diff x 'exp(sin(x * x))'
  f         exp(sin(x * x))
  df/dx     exp(sin(x * x)) * (cos(x * x) * (x + x))
```

`d(x, e)` is an ordinary node with ordinary rules. A conventional symbolic
differentiator emits a correct but grotesque expression and then runs a
simplifier over it, and whatever the simplifier misses, you keep. Here the
derivative rules and the algebraic rules saturate *together*: every
intermediate form of the derivative is visible to every algebraic rule at
once, and extraction picks the cheapest final answer out of all of them.

### It compiles

<!-- DEMO:vm -->

```console
$ saturn vm 'a*x^3 + b*x^2 + c*x + d' --rules all
  params    p0=a p1=b p2=c p3=d p4=x
  consts    (none)
  11 instructions, 5 slots

     0  s0   = p4            ; x
     1  s1   = p2            ; c
     2  s2   = p1            ; b
     3  s3   = p0            ; a
     4  s4   = *      s0, s3
     5  s3   = +      s2, s4
     6  s4   = *      s0, s3
     7  s3   = +      s1, s4
     8  s4   = *      s0, s3
     9  s3   = p3            ; d
    10  s0   = +      s4, s3

  result in s0
```

<!-- DEMO:time -->

```console
$ saturn time 'a*x^3 + b*x^2 + c*x + d' --rules all
  input     a * x ^ 3 + b * x ^ 2 + x * c + d
  optimized x * (c + x * (b + x * a)) + d

                          ns/eval   speedup
  interpreted                830.0   1.0x
  compiled                    49.6   16.7x
  compiled + optimized        19.2   43.2x

  program 15 -> 11 instructions, 5 -> 5 slots, 211 rules from `all`
```

That is the Horner form from above, compiled. The extracted expression is
already a maximally shared DAG, so common-subexpression elimination is not a
pass — it is a consequence of how the expression is represented. Slots are
recycled once a value is dead, so eleven instructions need five slots rather
than eleven.

The cost model said 91% cheaper; the machine says 2.7x, on top of the 13x that
compiling buys on its own. A cost model is a guess about hardware — `saturn
time` is the hardware answering.

### It emits code

<!-- DEMO:emit -->

```console
$ saturn emit 'a*x^3 + b*x^2 + c*x + d' --rules all --name poly
#include <math.h>

double poly(double a, double b, double c, double d, double x) {
    return x * (c + x * (b + x * a)) + d;
}
```

C, Rust, or Python. Subterms used more than once become temporaries; anything
the target spells differently — `min` and `max`, whose standard versions leave
the tie between `+0.0` and `-0.0` unspecified, `sign`, which no target has,
and in Python `/`, `pow`, `sqrt`, `log` and the rest, which raise where
IEEE-754 returns a value — gets a small helper that restores the double's
behaviour.

That list is not from reading the standards. The test suite compiles the
emitted C and Rust, runs the emitted Python, and compares 120 random
expressions over 24 hostile input rows against the reference interpreter,
**bit for bit**. Every item on it was a failure first.

### It optimizes several formulas at once

Optimizing formulas one at a time throws away the thing they most often have
in common: each other. `--file` puts them in one e-graph, so a subterm two
outputs use is found once, extracted once, and emitted once:

<!-- DEMO:bundle -->

```console
$ saturn emit --file examples/formulas/quadratic.txt --rules all --name roots
#include <math.h>

void roots(double a, double b, double c, double *lo, double *hi) {
    const double t0 = sqrt(b * b - 4.0 * a * c);
    const double t1 = a + a;
    *lo = (-b - t0) / t1;
    *hi = (t0 - b) / t1;
}
```

The input repeats `sqrt(b*b - 4*a*c)` and `2*a` in both roots; nothing tells
the engine they are the same, and it would still find the sharing if they were
written differently. `saturn opt --file` reports the cost together and the
cost apart, so the difference is visible rather than claimed.

### It checks itself

<!-- DEMO:fuzz -->

```console
$ saturn fuzz --rules safe --count 400 --samples 200
  fuzzing 400 expressions, depth 5, 113 rules from `safe`, tolerance 0

  clean every optimized expression agreed with its input
```

Every one of those expressions was saturated, extracted, and then evaluated
against its original over 200 inputs drawn from a distribution designed to
break float code. At tolerance zero.

```console
$ saturn fuzz --rules safe --count 400 --samples 200
  fuzzing 400 expressions, depth 5, 102 rules from `safe`, tolerance 0

  clean every optimized expression agreed with its input
```

---

### It takes facts you give it

The analysis can only prove what the expression implies, which for a bare
variable is nothing: `w / w` never cancels, because `w` might be zero,
infinite or NaN. Say otherwise and eleven guarded rules come unstuck at once:

<!-- DEMO:assume -->

```console
$ saturn opt 'w / w * max(x, 0)' --assume 'finite(w) && nonzero(w), x > 0'
  input     w / w * max(x, 0)
            6 nodes, 20.375 ops

  optimized x
            1 nodes, 0.125 ops  99% cheaper

  e-graph   4 classes, 7 nodes, 3 iterations, 140.2µs (saturated)
  rules     146 rules from `default`
```

Constraints are ranges (`x > 0`, `-1 <= t, t <= 1`, `x == 3`) plus two
predicates a range cannot express — `finite(x)` bounds a value without saying
where, and `nonzero(x)` is a hole in the middle of a range rather than a range.

Assumptions are taken on trust. A false one makes the result wrong in exactly
the way a fast-math rule would, which is why you have to ask for them.

## Floating point is not algebra

The rule library is split into two tiers, and the split is load-bearing.

**`safe`** rules preserve the exact IEEE-754 result for every input, including
`±0.0`, `±inf`, and NaN — either because the identity holds bit for bit, or
because a side condition rules out the cases where it would not.

`?x / ?x => 1` is *wrong*: `x` may be zero, infinite, or NaN. It is in `safe`
anyway, guarded by `is_finite_nonzero("?x")`, and an interval analysis attached
to every e-class is what discharges that guard. A rule that cannot prove its
side condition simply does not fire.

**`fast-math`** rules are true over the reals and false over floats:
reassociation, distribution, `ln(exp(x)) => x`. They are the same trade a C
compiler makes under `-ffast-math`, and nothing enables them unless you ask.

Every safe rule is tested as a claim, not just as part of a system. Rules with
no side condition are evaluated against their own right-hand side over
thousands of hostile inputs at tolerance zero. Conditional rules are built in a
real e-graph and the analysis is asked the same question the rule asks —
the identity is then checked exactly where the rule would fire, which is the
only place it has to hold. All 103 are covered, and the test reports what it
could not reach rather than letting a green tick imply coverage.

`saturn fuzz` is how this stays honest rather than aspirational. It generates
random expressions, optimizes them, and compares against the original over
inputs including subnormals, infinities, and magnitudes spanning eighty orders
of magnitude — at tolerance **zero** for the safe tier.

---

## The language

```
x  y  z             variables
1  1.5  1e-9  pi    literals
a + b   a - b   a * b   a / b   a ^ b   -a
a < b   a <= b  a > b   a >= b  a == b  a != b
a && b  a || b  !a                          0.0 is false, anything else true
sqrt ln exp sin cos tan abs sign floor ceil
min(a, b)   max(a, b)   atan2(a, b)   pow(a, b)
if(c, a, b)
d(x, e)                                     the derivative of e by x
let t = e1 in e2                            inlined at parse time
# comments run to end of line
```

`let` is substituted away immediately. The language is pure, so inlining
changes nothing, and hashconsing recovers the sharing exactly — which keeps
binders out of the e-graph entirely.

---

## Command line

| command | |
|---|---|
| `saturn opt <expr>` | saturate and extract the cheapest equivalent expression |
| `saturn eval <expr> -D x=1.5` | evaluate |
| `saturn diff <var> <expr>` | differentiate, simplifying as it goes |
| `saturn why <expr> <expr>` | show the chain of rules that proves the two equal |
| `saturn check <expr>` | compare the optimized form against the original numerically |
| `saturn fuzz` | generate random expressions and test the rules for soundness |
| `saturn vm <expr>` | compile to bytecode and disassemble |
| `saturn emit <expr>` | print the optimized expression as C, Rust, or Python |
| `saturn opt --file <path>` | optimize several named expressions together |
| `saturn time <expr>` | measure interpreted, compiled, and optimized evaluation |
| `saturn ast <expr>` | show the parsed expression DAG |
| `saturn egraph <expr>` | dump the saturated e-graph, or `--dot` for Graphviz |
| `saturn rules [set]` | list the rule sets, or the rules in one |
| `saturn bench` | run the built-in suite and report the savings |
| `saturn repl` | interactive |

Useful flags: `--file <path>`, `--rules <set>`, `--assume '<facts>'`, `--cost size|depth|ops`,
`--iters`, `--nodes`, `--time`, `--why`, `--stats`, `--shared`. `saturn --help` has the rest. Setting
`SATURN_TRACE=1` streams each phase to stderr as it happens.

<!-- DEMO:rules -->

```console
$ saturn rules
  rule sets
  safe              113  every rule that preserves IEEE-754 results exactly
  default           146  safe + differentiation (the default)
  diff               33  symbolic differentiation only
  arith              23  safe arithmetic identities
  transcendental     17  safe exp/log/pow/sqrt/trig identities
  logic              73  safe comparison, boolean, if, min/max/abs
  fast-math          65  real-valued identities that change float results
  all               211  default + fast-math
  none                0  no rules; just parse, fold constants, and extract

  list one with `saturn rules <name>`
```

<!-- DEMO:bench -->

```console
$ saturn bench
  using 211 rules from `all`

  name           nodes -> nodes     ops -> ops      classes    time
  identity         7 -> 1           10 -> 0            839   175.1ms
  factor           9 -> 7           14 -> 6             15   346.2µs
  cancel           5 -> 3           20 -> 1            902    95.9ms
  powers           5 -> 5          160 -> 16          1177   343.4ms
  exp-fuse         8 -> 6          143 -> 47            14   334.9µs
  log-ratio        5 -> 4           91 -> 60             8   173.5µs
  trig             6 -> 1          129 -> 0              7   125.3µs
  horner          15 -> 11         176 -> 16            32   935.8µs
  divide           6 -> 5           31 -> 16             7   117.4µs
  deriv            9 -> 8            - -> 8           1303   365.0ms
  deriv-chain      5 -> 8            - -> 178          839   303.3ms
  sqrt-square      7 -> 6           49 -> 26             8   150.1µs
  boolean          8 -> 6            6 -> 4              8   143.4µs
  big              7 -> 7           10 -> 10           557    37.3ms

  overall 76% cheaper (derivatives excluded: they have no runtime cost to compare against)
```

Not every line improves, and that is the cost model doing its job.
`ln(a) - ln(b)` becomes `ln(a / b)` because one logarithm beats two. `(x^2)^3`
becomes a chain of multiplies because a `pow` call costs more than five of
them. `(a + b + c)^3` is left exactly as written, because it already is the
cheapest thing in its e-class -- and being told that is worth something too.

### Cost models

| model | measures | picks |
|---|---|---|
| `size` | one unit per node | the smallest expression |
| `depth` | longest path | the shortest critical path |
| `ops` | rough hardware latency | the fastest to evaluate |

`ops` is the default and the interesting one. It prices a divide at 15 adds, a
square root at 20, and a transcendental at 45–80, which is roughly the shape of
real hardware. That is what makes `u/w + v/w` worth turning into `(u+v)/w`, and
`exp(a) * exp(b)` worth turning into `exp(a+b)`: both trade an expensive
operation for a cheap one, and neither is smaller.

---

## Library

One call for the whole pipeline:

```rust
use saturn::{optimize, parse, rules, OpCost};

let expr = parse("u / w + v / w")?;
let (best, runner) = optimize(&expr, &rules::all_rules(), OpCost);
println!("{}", best.pretty());              // (u + v) / w
println!("{}", runner.report());            // what stopped it, and which rules fired
```

Or drive the pieces yourself, which is what you want as soon as you care about
limits, a second cost model, or the e-graph itself:

```rust
use saturn::{parse, rules, Runner, Extractor, AstSize, OpCost};
use std::time::Duration;

let expr = parse("u / w + v / w")?;
let runner = Runner::default()
    .with_expr(&expr)
    .with_node_limit(50_000)
    .with_time_limit(Duration::from_secs(1))
    .run(&rules::all_rules());

// One saturation, as many answers as you have cost models.
let (_, fastest)  = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
let (_, smallest) = Extractor::new(&runner.egraph, AstSize).find_best(runner.root());
```

Writing rules:

```rust
use saturn::{rw, rules::is_finite_nonzero, Rewrite, MathAnalysis};

let rules: Vec<Rewrite<MathAnalysis>> = vec![
    rw!("factor";  "?a * ?b + ?a * ?c" => "?a * (?b + ?c)"),
    rw!("cancel";  "?a / ?a"           => "1",
        if "?a is finite and nonzero", is_finite_nonzero("?a")),
];
```

Bringing your own analysis:

```rust
use saturn::{Analysis, EGraph, ENode, Id};

struct CountLeaves;

impl Analysis for CountLeaves {
    type Data = usize;
    fn make(egraph: &EGraph<Self>, node: &ENode) -> usize {
        if node.children.is_empty() {
            1
        } else {
            node.children.iter().map(|&c| egraph[c].data).sum()
        }
    }
    fn merge(&mut self, a: usize, b: usize) -> usize {
        a.min(b)
    }
}
```

---

## Layout

| | |
|---|---|
| `src/sym.rs` | string interning; a total-order, hashable `f64` key |
| `src/lang.rs` | operators, e-nodes, the maximally-shared expression DAG |
| `src/lexer.rs` `src/parser.rs` | tokenizer and Pratt parser |
| `src/unionfind.rs` | path-halving union-find |
| `src/egraph.rs` | hashcons, congruence, deferred rebuilding, invariant checks |
| `src/interval.rs` | the interval abstract domain |
| `src/analysis.rs` | the analysis trait; constant folding over intervals |
| `src/assume.rs` | facts the caller supplies about the variables |
| `src/pattern.rs` | patterns and e-matching |
| `src/rewrite.rs` | rules, conditional and dynamic appliers, the `rw!` macro |
| `src/runner.rs` | the saturation loop and the backoff scheduler |
| `src/extract.rs` | cost models and extraction |
| `src/explain.rs` | derivations: why two expressions are equal |
| `src/rules/` | the rule library, split by tier |
| `src/vm.rs` | bytecode compiler and register machine |
| `src/eval.rs` | the reference interpreter |
| `src/bundle.rs` | several named expressions sharing one DAG |
| `src/codegen.rs` | emitting C, Rust, and Python |
| `src/fxhash.rs` | a fast hasher for the engine's own maps |
| `src/gen.rs` `src/check.rs` | random expressions and differential testing |

---

## Honest limitations

- **No binders in the e-graph.** `let` is inlined. Equality saturation over a
  language with real binders is a substantially harder problem.
- **Extraction is optimal for tree cost, not DAG cost.** Minimizing cost over
  the shared DAG — counting a subterm used twice only once — is NP-hard.
  `DagExtractor` takes a greedy pass and says so.
- **The interval domain is simple.** No relational information, so it cannot
  prove `x - x` finite from `x` being finite twice over; no quadrant analysis
  for trigonometry.
- **Saturation is not guaranteed.** Many rule sets grow without bound. That is
  why the limits exist, and why `--stats` tells you whether you got a proof or
  a timeout — the difference matters, because a saturated run means the result
  is optimal for the rules and cost model given.

---

## Prior art

The e-graph, congruence closure, and deferred rebuilding come from
[egg](https://egraphs-good.org) (Willsey et al., POPL 2021), which in turn
builds on Nelson and Oppen's congruence closure and on Tate et al.'s
*Equality Saturation* (POPL 2009). The float-soundness tiering is closest in
spirit to [Herbie](https://herbie.uwplse.org).

`saturn` is an independent implementation written to be read: no dependencies,
no macros beyond one convenience, and every non-obvious decision explained
where it is made.

## License

MIT.
