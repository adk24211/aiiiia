# Making money from this

An honest plan, with the parts I am not certain about marked as such.

## Who pays, and for what

Not for the delivery. Delivery is the part a competent team can write in a
week, and the part this repository gives away. People pay for the week they
did not spend, and then they keep paying for the things that only show up in
month six:

| What | Who feels it |
|---|---|
| A customer says "we never got the event" and you can prove otherwise | support, weekly |
| A consumer was down for an hour and wants the hour back | the customer's engineer, occasionally, loudly |
| Rotating a signing secret across 4,000 consumers without a cutover | security, once a year and under duress |
| One customer's dead endpoint stops the queue for everyone else | your on-call, once, memorably |
| An auditor asks for six months of delivery evidence | compliance, at the worst time |

The wedge is the audit trail and replay, not the retry loop.

## The shape: open core

The whole delivery path is MIT and stays MIT. That is not generosity, it is
distribution: a single binary with no dependencies gets run by people who
would never fill in a contact form, and some fraction of them grow into the
paid tier.

A defensible split, in roughly the order teams start asking for each:

**Free, forever, in this repository.** Delivery, retries, signatures,
rotation, the breaker, replay, the audit trail, the admin UI, SSRF defence.
Everything a single team needs to ship webhooks properly.

**Paid.** The things that only matter once more than one person operates it,
or once someone outside engineering has an opinion:

* **Single sign-on and roles** for the admin UI. This is the classic first
  paid feature for a reason: it is worth nothing to a solo operator and
  mandatory to a company with a compliance function.
* **A consumer-facing portal.** Your customers log in, see *their* deliveries,
  replay their own failures, rotate their own secret. This is the feature that
  removes the support ticket rather than answering it faster, and it is the
  one I would build first.
* **Multi-node.** One process per database is a real limit. Horizontal
  workers, Postgres as an alternative store, and a way to run three of these
  behind a load balancer.
* **Retention and export.** Attempts to S3, a query API over the archive, a
  retention policy per tenant.
* **Alerting integrations.** "Tell PagerDuty when a customer's endpoint has
  been failing for an hour" — trivial to build, disproportionately valued.

Keep the paid surface at the edges. Anything that touches whether a webhook is
delivered correctly stays free, because the moment correctness is a paid tier,
the free tier is a liability rather than an advertisement.

## Pricing

I would not price per event. Event volume has no relationship to the value
here — a customer sending ten million machine events feels less pain than one
sending ten thousand events to four hundred flaky consumers — and per-event
pricing punishes exactly the growth you want.

Price on **endpoints under management**, or on seats for the portal. Both
track the thing that actually generates support load.

A starting shape, to be tested rather than believed:

* **Free** — self-hosted, unlimited, MIT.
* **Team** — the paid features, self-hosted, per year, flat. Aim at the number
  a team lead can approve without a procurement cycle.
* **Business** — same, plus the portal and SSO, priced by endpoints.
* **Hosted** — you run it. Highest margin, highest operational cost, and the
  thing to do *last*, once support volume tells you what breaks.

I do not have current, reliable numbers for what the commercial alternatives
charge, and I am not going to invent them. Before setting a price, read the
public pricing pages of the hosted webhook services and of two or three
open-core infrastructure companies of a similar shape, and anchor against
those.

## Why this can win a deal

Against a hosted service: **your customers' payloads never leave your
infrastructure.** For anyone in payments, health, or a regulated market in the
EU, that is not a preference, it is the reason the hosted option was
eliminated in the first meeting. A single self-hosted binary with no
dependencies is a much shorter security review than a vendor.

Against building it: the three bugs in this repository's history are the
argument. They are not exotic — a deferred SQLite transaction that cannot be
retried, a query parameter silently rejected behind a flattened struct, a
breaker that swallowed an operator's replay — and every one of them would have
been found in production by a team writing this themselves. That list, kept
public and kept growing, is the most persuasive page on the site.

Against the other open-source options: signature compatibility means switching
costs a consumer nothing, and a consumer that has to change code is a switch
that does not happen.

## What to do next, in order

1. **Publish it.** A README that shows a working request in the first screen.
   The `docs/consumers.md` snippets are the adoption lever: they are the first
   code a customer of *your* customer ever runs, and every one of them is
   tested rather than written.
2. **A hosted demo** that anyone can send an event to and watch arrive. The
   admin UI is already the demo; it needs a URL.
3. **Client libraries** for sending — Node, Python, Go. Thin wrappers, an
   afternoon each, and they are what turns a reader into a user.
4. **The consumer portal.** The first thing worth charging for, and the first
   thing a prospect asks for after they have run it for a month.
5. **Then** SSO, then multi-node, then hosted. In that order, and not before
   someone has asked.

## The honest risks

* **The market has incumbents**, hosted and open-source, some well funded.
  Self-hosting and licence terms are the differentiation; features are not.
* **Open core annoys people.** Draw the line once, publish it, and never move
  it inward. Moving a free feature behind a paywall costs more trust than the
  feature earns.
* **One process per database is a real ceiling.** It is the right trade for
  most deployments and the wrong one for a few, and the few are the ones with
  budget. Multi-node is the first thing that will be demanded.
* **Support is the product** in this category, and support does not scale by
  writing more code.

## A note on the licence

This is MIT today, which is the most permissive reasonable choice and the best
one for adoption. If you later want the open part to stay open — a competitor
cannot take it and offer a hosted version without contributing back — the
usual choices are AGPL-3.0 or a source-available licence such as BSL 1.1 with
a change date.

Changing the licence of the existing code needs the agreement of everyone who
has contributed to it, which is easiest now, while that is one person. If you
intend to change it, do it before accepting the first outside pull request, or
start requiring a contributor licence agreement from the first one.

See [licensing.md](licensing.md) for the mechanics.
