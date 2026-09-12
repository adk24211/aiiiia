# saturn

**Equality saturation for a small numeric language.** An optimizer that does
not have to guess which rewrite to apply, because it applies all of them.

Zero dependencies. Rust 1.74+. Library and CLI.

```
cargo run -- opt 'u / w + v / w'
cargo run -- diff x 'exp(sin(x * x))'
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

`saturn` implements that from first principles: e-graph with congruence
closure and deferred rebuilding, e-matching, an interval-based e-class
analysis that discharges side conditions, a backoff rule scheduler, optimal
cost extraction, a symbolic differentiator written entirely as rewrite rules,
and a bytecode compiler for the result.

`docs/design.md` is the full tour.

---

## What it does

<!-- DEMO:opt -->

Nothing in the rule library knows about that expression. The engine found the
factoring because the e-graph held `u/w + v/w` and `(u+v)/w` simultaneously and
the cost model priced a divide at fifteen adds.

### Differentiation is just more rules

<!-- DEMO:diff -->

`d(x, e)` is an ordinary node with ordinary rules. A conventional symbolic
differentiator emits a correct but grotesque expression and then runs a
simplifier over it, and whatever the simplifier misses, you keep. Here the
derivative rules and the algebraic rules saturate *together*: every
intermediate form of the derivative is visible to every algebraic rule at
once, and extraction picks the cheapest final answer out of all of them.

### It compiles

<!-- DEMO:vm -->

The extracted expression is already a maximally shared DAG, so
common-subexpression elimination is not a pass — it is a consequence of how
the expression is represented. Slots are recycled once a value is dead, so a
long dependency chain needs a handful of them rather than one per node.

### It checks itself

<!-- DEMO:fuzz -->

---

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
| `saturn check <expr>` | compare the optimized form against the original numerically |
| `saturn fuzz` | generate random expressions and test the rules for soundness |
| `saturn vm <expr>` | compile to bytecode and disassemble |
| `saturn ast <expr>` | show the parsed expression DAG |
| `saturn egraph <expr>` | dump the saturated e-graph, or `--dot` for Graphviz |
| `saturn rules [set]` | list the rule sets, or the rules in one |
| `saturn bench` | run the built-in suite and report the savings |
| `saturn repl` | interactive |

Useful flags: `--rules <set>`, `--cost size|depth|ops`, `--iters`, `--nodes`,
`--time`, `--stats`, `--shared`. `saturn --help` has the rest.

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

```rust
use saturn::{parse, rules, Runner, Extractor, OpCost};

let expr = parse("u / w + v / w")?;
let runner = Runner::default().with_expr(&expr).run(&rules::safe());
let (cost, best) = Extractor::new(&runner.egraph, OpCost).find_best(runner.root());
println!("{}  (cost {})", best.pretty(), cost);
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
| `src/pattern.rs` | patterns and e-matching |
| `src/rewrite.rs` | rules, conditional and dynamic appliers, the `rw!` macro |
| `src/runner.rs` | the saturation loop and the backoff scheduler |
| `src/extract.rs` | cost models and extraction |
| `src/rules/` | the rule library, split by tier |
| `src/vm.rs` | bytecode compiler and register machine |
| `src/eval.rs` | the reference interpreter |
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
