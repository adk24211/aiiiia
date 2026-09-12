# How saturn works

This is a tour of the implementation, in the order the data flows. It assumes
no prior knowledge of e-graphs.

## The problem with rewriting

A conventional optimizer holds one program and rewrites it in place. That means
it must decide, at every step, which rule to apply — and the decision is
destructive. Consider

```
(a + b) * (a + b) / (a + b)
```

To cancel the division you want to keep the multiplication factored. To fold
`a + b` into a single temporary you want the opposite. A rewriter that
distributes first can no longer cancel; a rewriter that cancels first may miss
a better factoring downstream. This is the **phase-ordering problem**, and the
usual answers — fixed pass pipelines, running passes to a fixpoint, hand-tuned
heuristics — are all ways of guessing well rather than of not having to guess.

Equality saturation removes the guess. Instead of replacing a term with its
rewrite, it records that the two are *equal*, and keeps both. Do that until the
rules run out of new equalities, and you have a data structure holding every
program the rules can reach. Only then do you choose, with full knowledge of
the alternatives.

The catch is that "every program the rules can reach" is usually infinite, and
always astronomically large. The e-graph is what makes it fit in memory.

## The e-graph

An **e-graph** is a set of equivalence classes of terms. Each *e-class* holds
some *e-nodes*; each e-node is an operator applied to a list of *e-classes*
rather than to other nodes.

That indirection is the whole trick. If `x * 2` and `x + x` are in the same
e-class, then any node referring to that class refers to both at once. One
e-class can stand for exponentially many terms: a chain of `n` classes each
holding two nodes represents `2^n` distinct expressions in `O(n)` space.

Two invariants make it work:

- **hashcons** — each e-node appears in exactly one e-class, and a hash map
  finds it in constant time. Adding a term that is already present costs
  nothing and creates nothing.
- **congruence** — if two e-nodes have the same operator and pairwise
  equivalent children, they are in the same e-class. This is what propagates
  an equality *upward*: proving `a = b` automatically proves `f(a) = f(b)`,
  and `g(f(a), c) = g(f(b), c)`, without anyone applying a rule.

`src/egraph.rs` maintains both. Equivalence itself lives in a union-find
(`src/unionfind.rs`) with path halving and union by size.

### Deferred rebuilding

Restoring congruence after every single union is correct and slow: a union can
force its parents to merge, which forces *their* parents to merge, and the
cascade is re-walked from scratch each time. `saturn` follows the *egg* paper's
deferred rebuilding instead. `union` does the minimum — merge the classes,
push the survivor onto a worklist, mark the graph dirty — and `rebuild()`
drains the worklist afterwards, repairing each class's parents once. Congruence
holds only *between* rebuilds, which is exactly when anyone looks.

`rebuild` then does something the paper does not: it reconstructs the three
derived indices (class node lists, the hashcons, and the parent lists) from
scratch in one linear pass. Maintaining those incrementally under interleaved
unions is where e-graph implementations grow their subtlest bugs — a stale key
that shadows a live one, a parent list that lost an entry to a merge. The
semantically interesting work, discovering new congruences, still happens
incrementally in `repair`; only the bookkeeping is redone. One linear pass per
rebuild costs far less than the e-matching that follows it.

`EGraph::check_invariants` verifies all of this and is called by the test suite
and by `saturn egraph`.

### Rebuilding terminates; the analysis might not

Congruence closure always finishes: every union reduces the number of
e-classes, and there are finitely many. An e-class analysis has no such
guarantee. Intervals have infinite descending chains — `[0, 1]`, `[0, ½]`,
`[0, ¼]` — and an e-graph's parent relation can be cyclic, so a class can feed
its own refinement forever.

Worse, the step function has to be *monotone* for a fixpoint to exist at all.
`Interval::meet` originally widened when two facts contradicted each other,
which let the analysis move back *up* the lattice by an arbitrary amount; a run
that should have taken milliseconds never returned. It now jumps to TOP, the
one value with nothing above it to jump to next. `rebuild` additionally caps
how many times it will re-run the analysis, which is a widening by fiat:
giving up on precision is sound, because every fact still in place was computed
from the facts below it. Giving up on *congruence* is not, so the worklist is
always drained before returning.

### Commutativity is structural

`a + b` and `b + a` denote the same value, and the usual way to say so is a
rewrite rule. That rule matches everywhere, fires constantly, and roughly
doubles the graph.

`saturn` instead stores commutative e-nodes with their children in a canonical
order, so `a + b` and `b + a` hashcons to the *same node*. The matcher pays for
this by trying both orders when it descends through a commutative operator —
a bounded, local cost — and commutativity is otherwise free.

`Op::is_commutative` lists which operators qualify, and every one of them had
to be checked against IEEE-754 rather than against intuition. `min` and `max`
failed. `f64::min` and C's `fmin` return "either input" when the two compare
equal, so on `+0.0` and `-0.0` the answer depends on operand order, on which
instruction the compiler chose, and on whether the operands happened to be
constants — which meant constant folding could disagree with the interpreter
about the same expression. `lang::min` and `lang::max` settle the tie
explicitly, and the code emitter carries a helper rather than trusting the
target's version. The bug surfaced only when the emitted C was compiled and
run against the interpreter; it survived every amount of reading.

## E-class analyses

Some facts are better computed than rewritten. `src/analysis.rs` defines a
semilattice attached to every e-class:

- `make(node)` computes the fact for one e-node from its children's facts,
- `merge(a, b)` combines two facts about the same value,
- `modify(class)` may act on the graph when a class's fact changes.

`MathAnalysis` carries two things. The first is a **constant**: if every child
of a node is a known literal, the node folds, and `modify` unions the class
with the literal. Constant folding therefore needs no rules at all, and it
happens *transitively* — proving a class equal to a constant folds everything
above it.

The second is an **interval** (`src/interval.rs`), a sound over-approximation
of the value with a separate NaN flag. Every transfer function rounds outward
by one ULP, so the concrete result is always inside the abstract one.

The interval is what makes float-safe rewriting possible. `?x / ?x => 1` is
wrong when `x` is zero, infinite, or NaN. Guarded by `is_finite_nonzero("?x")`,
it is exactly right, and the interval is what discharges the guard. The analysis
is a *fact provider* for the rules, and a rule that cannot prove its side
condition simply does not fire.

### Facts from outside

An analysis that can only see the expression proves very little about a bare
variable, and `?x / ?x => 1` is stuck for good. `src/assume.rs` lets the caller
supply what they know — `x > 0`, `finite(w)`, `nonzero(w)` — and the rules that
were waiting on a proof come unstuck.

Two details are load-bearing.

Assumptions live in the analysis, not in the class. Writing a tighter interval
into an e-class once would be erased the first time anything below it changed,
because the analysis recomputes a class from its nodes. A variable's fact has
to be part of what the variable *means*, which is `Analysis::make` for
`Op::Var`.

`nonzero` is a separate bit on the interval rather than a range. "Anything but
zero" is a hole in the middle of an interval, and this domain has no holes. It
propagates through exactly the operations that cannot turn a non-zero into a
zero — negation, absolute value, square root, sign — and not through
multiplication, because two non-zero values can underflow to zero.

Assumptions are taken on trust, and the docs say so: a false one makes the
result wrong in exactly the way a fast-math rule would.

## Patterns and e-matching

A pattern is an expression with variables (`?x`). Matching it against an
e-class asks: in how many ways can the variables be bound so that the pattern
denotes a term in this class? Because a class stands for many terms, the answer
is usually "several", and `src/pattern.rs` returns all of them.

The matcher is a backtracking walk. Descending into an e-class tries every node
with the right operator; descending into a child threads the substitution
through. A variable that appears twice must bind to the same class both times,
which is what makes `?x - ?x => 0` sound — it fires only when both sides are
*provably* the same value, not merely spelled the same.

## The saturation loop

`src/runner.rs` alternates two phases:

1. **search** every rule against a frozen e-graph, collecting matches;
2. **apply** every match, then `rebuild`.

They are separate on purpose. If a rule could match a term another rule created
in the same pass, one rule could run away inside a single iteration, and the
result would depend on rule order. Freezing the graph during search makes a run
reproducible.

The loop stops when it is **saturated** — a full pass found nothing new, which
means the e-graph now provably contains every term the rules can reach — or
when it hits a limit on iterations, nodes, or time. `saturn opt --stats` reports
which happened; the distinction matters, because a saturated run means the
extracted result is optimal *for the rules and cost model given*, and a
truncated one means only that it was the best found so far.

### Rule scheduling

Left alone, associativity and distribution crowd out everything else: they
match a growing number of times each iteration and consume the node budget
before the rules that actually shrink the expression get a turn. The
`BackoffScheduler` watches each rule's match count, bans one that exceeds its
limit for a few iterations, and doubles both the limit and the ban length each
time. Expensive rules still run — just rarely, and after the cheap rules have
shaped the graph. When the loop would otherwise declare saturation while some
rule is banned, the scheduler unbans everything and demands one more pass, so a
ban can never be mistaken for a proof.

## Extraction

Saturation leaves an enormous set of equivalent programs. Extraction picks one,
guided by a cost function (`src/extract.rs`).

The algorithm is a bottom-up fixpoint: the cost of a class is the minimum over
its nodes of the node's own cost plus its children's costs, iterated until
nothing changes. Costs only decrease and are bounded below, so it terminates,
and it is **optimal** for any cost that sums over the expression *tree*.
Classes whose every node sits in a cycle with no grounded base case simply
never get a cost, which is the right answer: no finite term in that class
exists.

Three cost models ship:

| model   | measures                | good for                          |
|---------|-------------------------|-----------------------------------|
| `size`  | one unit per node       | smallest expression               |
| `depth` | longest path            | shortest critical path            |
| `ops`   | rough hardware latency  | fastest to actually evaluate      |

`ops` is the interesting one. It prices a divide at 15 adds, a square root at
20, and a transcendental at 45–80, which is roughly the shape of real hardware.
That is what makes `u / w + v / w` worth turning into `(u + v) / w`, and
`exp(a) * exp(b)` worth turning into `exp(a + b)`: both trade an expensive
operation for a cheap one, and neither is smaller.

### The honest caveat

Minimizing cost over the expression *tree* is what the fixpoint solves
optimally. Minimizing it over the shared **DAG** — counting a subterm used
twice only once, which is what actually gets emitted — is a different problem,
and it is NP-hard. `DagExtractor` takes a greedy pass at it and says so in its
documentation. Where the two disagree, `saturn` reports DAG cost in the CLI,
because that is the number that corresponds to work the machine does.

## Limits that actually limit

A rule set that grows the graph without bound is normal, so the runner takes an
iteration, node, and time budget. Getting those to hold took three tries.

Checking between iterations is not enough: a single rule's e-matching can run
for minutes on a graph that just grew by two orders of magnitude. Checking
between rules is not enough either, because the matcher itself is unbounded —
and capping the *results* it returns does not help, since a pattern can descend
through thousands of e-nodes and fail at the last level every time, producing
nothing while spending everything. Only a counter on steps taken bounds that.

So there are three: the clock is read between rules and every 256 substitutions
during application, the matcher has a step budget, and a search that hits its
cap is reported in the iteration's `truncated` list rather than swallowed. A
truncated search is a reason the run is not a proof of saturation, and the
report says so.

## From e-graph to machine

`RecExpr` (`src/lang.rs`) is a flat, topologically sorted DAG built through a
hashconsing `add`, so structurally identical subterms are shared automatically.
Common-subexpression elimination is not a pass; it is a consequence of how the
expression is represented.

`src/vm.rs` compiles that DAG to a register machine. Each node gets a slot,
computed once; slots are recycled once their value is dead, so a long
dependency chain needs a handful of slots rather than one per node. The VM
performs the same operations in the same order as the reference interpreter in
`src/eval.rs`, and the test suite asserts they agree *bit for bit* — an
approximate check would hide exactly the kind of bug worth finding.

## Emitting code

`src/codegen.rs` turns the extracted DAG into a C, Rust, or Python function,
binding a temporary for every subterm used more than once.

The interesting part is how much of the target language cannot be used
directly. `min` and `max` leave the `±0` tie unspecified. `sign` does not
exist, and the obvious ternary gets NaN wrong. An unsuffixed Rust float literal
is an ambiguous numeric type, so `0.5.sqrt()` does not compile. Python's `/`,
`**`, `math.pow`, `sqrt`, `log`, `exp`, `sin`, `cos` and `tan` all *raise*
where IEEE-754 returns a NaN or an infinity, and `floor` and `ceil` return
integers. Each gets a small helper, emitted only when the expression needs it.

None of that list came from reading standards. The test suite compiles the
emitted C and Rust, runs the emitted Python, and compares 120 random
expressions over 24 hostile input rows against the reference interpreter bit
for bit. Every item was a failure first.

## Differentiation as rewriting

`d(x, e)` is an ordinary node with ordinary rules (`src/rules/diff.rs`). The
product rule, the chain rule and the rest push the derivative down toward the
leaves, where `d(x, x) => 1` and `d(x, e) => 0` for `e` independent of `x`
finish the job.

Doing this inside an e-graph is not merely a tidy encoding. A conventional
symbolic differentiator produces a correct but grotesque expression and then
runs a simplifier over it, and whatever the simplifier misses, you keep. Here
the differentiation rules and the simplification rules saturate *together*:
every intermediate form of the derivative is available to every algebraic rule
simultaneously, and extraction picks the cheapest final answer out of all of
them. The two phases are not ordered because there are no phases.

The "`e` does not mention `x`" side condition is the subtle part. An e-class is
reachable from many nodes and the graph may contain cycles, so deciding it
requires a fixpoint over the reachable subgraph, and the answer must be
conservative: reporting "might depend on `x`" when unsure merely misses an
optimization, while reporting "independent" wrongly produces a wrong
derivative.

## Explaining an equality

Saturation answers "are these the same?" but the answer is worth little
without the reasoning. Every union the e-graph performs records why: a rule
with the bindings it matched, a congruence with the two nodes involved, a
constant fold, or a bare assertion.

Connectivity in that list of recorded unions is exactly union-find
connectivity, so a path between two ids *is* a derivation of their equality,
and a breadth-first walk finds the shortest one for free.

Two things had to be true before this produced anything readable.

**Arguments pair by equivalence class, not by position.** Commutative children
are stored in a canonical order, so two congruent nodes routinely differ by a
swap. Pairing them positionally compares arguments that are not equal at all,
finds no derivation, and reports a congruence that needed no explanation —
which collapsed the entire Horner proof into one useless line.

**The analysis runs at rebuild, not inside `add`.** A fold performed while a
node is being added unions its class with the literal before the caller holds
a handle to either, leaving nothing for a derivation to connect. Deferring it
also stops the analysis acting on a half-built graph.

This is a *path*, not the full proof tree that egg constructs. Congruence
steps unfold recursively into their arguments, bounded by depth, which is what
a reader wants when a rule is under suspicion; a complete term-level proof
down to the leaves is a substantially larger construction and is not here.

Recording is off by default — the list grows with the number of unions, which
on a saturating run is far larger than the number of classes — and it is
observation only. A test asserts that a run with explanations enabled produces
the same graph and the same extracted result as one without, because otherwise
the derivation would describe a different run than the one it explains.

## Float soundness

The rule library is split into two tiers, and the split is load-bearing.

**Safe** rules preserve the exact IEEE-754 result for every input, including
`±0.0`, `±inf` and NaN — either because the identity holds bit for bit, or
because a side condition rules out the cases where it would not.

**Fast-math** rules are true over the reals and false over floats:
reassociation, distribution, `ln(exp(x)) => x`. They are the same trade a C
compiler makes under `-ffast-math`, and nothing enables them unless you ask.

`saturn check` exists to keep this honest. It evaluates the original and the
optimized expression over thousands of seeded random inputs — including
subnormals, infinities, and values spanning eighty orders of magnitude — and
reports the worst disagreement it found, with the exact input that produced it.

## What this is not

- There are no binders in the e-graph. `let` is inlined at parse time and the
  sharing it expressed is recovered exactly by hashconsing. Equality saturation
  over a language with real binders is a substantially harder problem.
- Extraction is optimal for tree cost, not DAG cost. See above.
- The interval domain is deliberately simple. It has no relational information,
  so it cannot prove `x - x` finite from `x` being finite twice over, and no
  quadrant analysis for trigonometry.
- Saturation is not guaranteed. Many rule sets grow the graph without bound,
  which is why the limits exist and why `--stats` tells you whether you got a
  proof or a timeout.
