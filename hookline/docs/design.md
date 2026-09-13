# Design

Why it is built this way, in the order the decisions were made.

## SQLite, and not a compromise

A webhook sender's working set is a queue and an audit log. Both are written
once and read by id. There is no query a separate database server would answer
faster than a local file, and the difference between "copy one file" and "run
Postgres" is the difference between something one person deploys in an
afternoon and something that needs a plan.

Three settings make it safe under concurrency:

* **WAL**, so readers never block the writer.
* **A busy timeout**, so a second writer waits instead of returning
  `SQLITE_BUSY`.
* **`synchronous = NORMAL`**, which under WAL still survives a process crash
  and only risks the last few commits on power loss — the right trade for a
  queue whose contents are retried anyway.

And one rule that is not a setting: **every transaction that will write starts
`IMMEDIATE`.** A deferred transaction takes a read lock and asks for the write
lock at its first `UPDATE`; if another connection wrote in between, SQLite
fails that upgrade with `SQLITE_BUSY` *immediately*, and the busy timeout does
not apply, because rolling back is the only way out. Taking the write lock up
front is the case the busy timeout was built for. This cost a real bug, which
is pinned by a test that fails on the old behaviour.

## The queue is a table with a lease

A worker claims due deliveries by writing a lease on them, in one statement:
the select, the update and what it returns are a single write, and SQLite
serialises writers, so two workers cannot take the same delivery.

The lease is what makes a crash safe. A worker that dies mid-request leaves
rows whose lease expires, and the next claim picks them up. Nothing has to
notice the worker died. The lease must outlast the request timeout — otherwise
a delivery still in flight is handed to a second worker and the consumer gets
it twice — and the server refuses to start if it does not.

One claimer hands work to a bounded pool of senders. The claim is deliberately
not concurrent: one writer at a time is the shape SQLite likes, and the
concurrency that matters is in the HTTP requests, which is where the time
goes. Posting a message wakes the claimer, so the poll interval is the latency
floor for retries, not for the common case.

## Retries: exponential, with full jitter

Base 5s, factor 4, capped at 6 hours, ten attempts — about twenty hours, which
covers an outage that starts on a Friday evening.

The jitter is full jitter: the delay is uniform on `[0, computed]`, not
`computed ± a bit`. This is the variant that measures best, and the reason is
that a hundred deliveries to the same endpoint all failed at the same moment
and would otherwise all retry at the same moment, forever, in a herd that
never disperses.

## The breaker, and why half-open matters

A retry schedule handles an endpoint that is briefly unwell. It handles an
endpoint that has been gone for a week much less well: every queued message
waits out the full schedule, and the queue fills with work that cannot
succeed.

So: five consecutive failures opens the circuit, and deliveries to an open
circuit are passed over at claim time rather than attempted — one dead
endpoint cannot occupy every worker. The cooldown doubles per further failure
up to half an hour, so an endpoint that has been gone for a day is probed
occasionally rather than constantly. After enough consecutive failures the
endpoint is disabled outright and a human has to turn it back on.

Half-open falls out of the cooldown: when it lapses, deliveries flow again,
and the first result either closes the circuit or opens it wider. Without a
half-open state a breaker either never reopens or reopens into a stampede.

One thing the breaker must not do is outlive the operator's knowledge. An
explicit replay closes the circuit, because the breaker's state is an
inference from past attempts and a replay is someone telling us the thing
those attempts failed against has been fixed — which cannot be inferred.

## Rate limits are spacing, not a bucket

An endpoint may cap how many deliveries a minute it will take. That is
enforced with one timestamp per endpoint: claiming a delivery moves the
endpoint's next allowed time forward by a minute divided by the limit. At six
a minute, one every ten seconds.

A token bucket would be the usual answer and is the wrong one here. A bucket
that has filled while an endpoint was quiet empties in one burst the moment
work arrives, which is exactly what an endpoint asking for a rate limit is
trying to avoid. Spacing has no burst to absorb, costs no counting, and makes
the rate exact rather than approximate.

Rate-limited endpoints are claimed in a second pass, separate from everyone
else's. That is not tidiness: a limited endpoint with ten thousand queued
deliveries would otherwise sit at the front of the queue, and every claim
would spend its budget looking at deliveries it is not allowed to take.

## Which failures are worth retrying

A 4xx is the endpoint saying the request is wrong. Sending the identical bytes
again gets the identical answer, and nine more attempts only add load to
something that has already said no. 408 and 429 are the exceptions: both are
about when the request arrived rather than what was in it.

Everything else — 5xx, a timeout, a refused connection — is retried.

## Signatures

[Standard Webhooks](https://www.standardwebhooks.com/), so a consumer who has
integrated with anyone else using it already has working code. The id and the
timestamp are inside the signature, so a captured request cannot be
re-pointed or moved forward in time.

Rotation works by signing with every active secret at once. A consumer moves
when they like; nobody has to be on a call. See
[signatures.md](signatures.md).

## SSRF is a first-class concern, not a validation

The addresses are chosen by users, and the requests come from inside your
network. The interesting part is not the list of forbidden ranges — that is
just a list — but *where the check lives*. hookline filters inside the DNS
resolver the HTTP client uses, so the address that is checked is the address
that is dialled, and there is no window between them to rebind in. Redirects
are not followed, and proxy environment variables are ignored, because both
would hand resolution to something that does not apply the policy.

See [security.md](security.md).

## Identifiers

Prefixed and time-sortable: `msg_06G9HHQQFTNK622Y5F55VEX13W`. The prefix means
a log line says what a thing is without a schema to hand, and passing an
endpoint id where a message id belongs is a 400 rather than an empty result.
Sortable means a cursor can be an id, which means paging does not shift when a
row is inserted, and `LIMIT ... OFFSET n` never has to count past rows it will
not return.

48 bits of millisecond timestamp, 80 bits of randomness, Crockford base32.

## What is deliberately absent

* **No transformation language.** A rule engine that rewrites payloads on the
  way out is a second program with no debugger. Send what you meant to send.
* **No fan-in.** This delivers your events to your customers. It is not an
  integration platform.
* **No TLS termination.** Every deployment already has something that does it,
  and a second certificate story is a second thing to renew.
* **No second datastore.** Adding Redis for the queue would double the number
  of things that can be down to make something already fast slightly faster.
