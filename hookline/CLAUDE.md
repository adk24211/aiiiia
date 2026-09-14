# Working on hookline

A webhook delivery service. `docs/design.md` explains why each piece is the
way it is; read it before changing the queue, the breaker or the sender.

This is **not** the saturn crate at the repository root, and the rules there do
not apply here. In particular, this crate has dependencies: it is a network
service, and hand-rolling TLS, HTTP and SQLite would be worse in every way
that matters. Adding one is still a decision to justify in the commit, not a
reflex.

## Ground rules

**A correctness claim is tested at the level it is claimed.** A signature that
verifies in a unit test and not over the wire is a bug that reaches customers,
so the delivery path is tested by running the real server against a real
consumer over real HTTP (`tests/delivery.rs`, `tests/security.rs`,
`tests/api.rs`). Three bugs in this repository's history were found that way
and are pinned by tests that fail on the old behaviour.

**Every transaction that writes starts `IMMEDIATE`.** Use `db::write_tx`, not
`conn.transaction()`. A deferred transaction that reads before it writes fails
its lock upgrade with `SQLITE_BUSY`, which the busy timeout cannot wait out.

**The tenant is a parameter, not a filter.** Every store function that reads an
endpoint, message or delivery takes the application it must belong to. Do not
add one that does not; a scope checked in a handler is a scope someone forgets
to check.

**The destination policy is enforced in the resolver.** Not before the request.
Checking a URL and then handing the name to the client leaves a window to
rebind in. If you touch `guard` or `sender`, the tests in `tests/security.rs`
are the specification.

**The lease must outlast the request timeout.** Otherwise a delivery still in
flight is handed to a second worker and the consumer receives it twice. The
server refuses to start when it does not; keep that check.

**A published snippet is run before it is published.** The consumer examples
in `docs/consumers.md` come from `examples/consumers/`, which
`examples/consumers/check.sh` runs against the specification's test vector. A
signature example that does not verify is worse than none.

## Before you commit

```
cargo fmt
cargo clippy --all-targets      # must be silent
cargo test --release            # must be green
./examples/consumers/check.sh   # when you touch signing or the snippets
```

## Things that have bitten

* **A deferred transaction cannot be retried.** See above. The test is
  `db::tests::a_transaction_that_reads_before_it_writes_waits_instead_of_failing`.
* **A query string has no types.** `#[serde(flatten)]` hands flattened fields
  on as the strings they arrived as, so a plain `Option<usize>` parses
  `?limit=2` on one listing and rejects it on another.
* **The breaker can outlive what it knows.** An explicit replay closes the
  circuit, because a replay is someone telling us the thing those attempts
  failed against has been fixed, which cannot be inferred.
* **A guard that writes before it refuses has enforced nothing.** Revoking the
  last signing secret checks first, then writes.
* **A rate limit is not a bucket.** A bucket that fills while an endpoint is
  quiet empties in one burst, which is what the limit was asked for to
  prevent.
* **A limit selects endpoints, so select the ones with work.** Batching over
  rate-limited endpoints that were merely *allowed* another delivery let thirty
  idle ones crowd out the one with a backlog.
* **Byte offsets are not character offsets.** `&text[..n]` panics when `n`
  lands inside a multi-byte character, and the strings here are user text: an
  event type, a hostname, a TLS library's message. In the sender that panic is
  the worst kind — it happens while a delivery is leased, so the lease expires,
  the delivery is retried, and it panics again for ever with no attempt
  recorded.
* **Zero is not a small number, it is a different mode.** A rate limit of zero
  parked every delivery for ever. Refuse it at the edge, and never let a stored
  one strand a queue.

## Writing

Comments explain why, never what. If a line needs a comment to say what it
does, rewrite the line. Every public item has rustdoc. No emoji. Nothing in
the repository refers to how it was written.
