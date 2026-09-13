# Signatures

hookline signs with [Standard Webhooks](https://www.standardwebhooks.com/), so
a consumer who has integrated with anyone else using the specification already
has working code, and the libraries that exist work unchanged.

## What is signed

```
webhook-id:        msg_06G9HHQQFTNK622Y5F55VEX13W
webhook-timestamp: 1789269571
webhook-signature: v1,x7y65XzTj7GJe4tTgYvc9nCVoWVlsQxooPKv1UniEhE=
```

The signed string is the message id, the timestamp and the body, joined with
full stops:

```
{webhook-id}.{webhook-timestamp}.{body}
```

HMAC-SHA256 with the base64-decoded secret, base64-encoded, prefixed `v1,`.

Three properties fall out of that, and all three matter:

* **The id is covered**, so a captured request cannot be re-pointed at another
  event.
* **The timestamp is covered**, so it cannot be moved forward to defeat the
  freshness check.
* **The body is covered exactly as sent**, which is why a consumer must verify
  against the raw bytes rather than a re-serialised object.

## Freshness

A consumer should refuse a timestamp more than a few minutes from now; five
is the usual tolerance and what `hookline verify` uses. Without it, a request
captured off the wire stays valid forever, because the signature never expires
on its own.

Check the timestamp *before* the signature. It is the cheaper of the two, and
when it fails it fails for a completely different reason — a clock that is
wrong, a queue that was paused — which the consumer's log should say.

## Rotation

An endpoint may hold several secrets. Every request is signed with all of the
active ones, and the header carries them space-separated:

```
webhook-signature: v1,<new> v1,<old>
```

A consumer that accepts any of them can move at its own pace. That is the
whole design: a rotation is not a cutover, and nobody has to be on a call.

```bash
# Add a new secret; the old ones stop signing in 24 hours by default.
curl -X POST $API/apps/$APP/endpoints/$EP/secrets/rotate \
     -H "authorization: Bearer $TOKEN" -d '{}'
# -> { "id": "sec_...", "secret": "whsec_...", "expires_at": null }

# Or set the window yourself.
curl -X POST $API/apps/$APP/endpoints/$EP/secrets/rotate \
     -H "authorization: Bearer $TOKEN" -d '{"grace_secs": 604800}'
```

A leaked secret is a different situation, where a grace period is the wrong
answer. Add a replacement, then revoke the leaked one outright:

```bash
curl -X POST   $API/apps/$APP/endpoints/$EP/secrets/rotate -d '{"grace_secs":0}'
curl -X DELETE $API/apps/$APP/endpoints/$EP/secrets/$LEAKED
```

An endpoint can never be left with no active secret; the revoke is refused
with a `409` rather than leaving it unable to sign anything.

## Bringing your own secret

If you are migrating from something else and your consumers already hold
secrets, pass them when creating the endpoint or rotating:

```json
{ "url": "https://consumer.example.com/hooks", "secret": "whsec_<theirs>" }
```

The `whsec_` prefix is optional and stripped; what is used as the HMAC key is
the base64-decoded remainder. A secret that is not valid base64 is used as its
raw bytes, so a migration from a scheme that used a plain string still works.

## Why not a JWT, or mutual TLS

A JWT would put the claims in the token and the body outside it, which means
signing the body separately anyway — the same HMAC with more moving parts and
an algorithm field that has its own decade of vulnerabilities.

Mutual TLS is stronger and is the right answer when both sides are yours. It
is not the right answer when the other side is a customer who has to get a
certificate into their load balancer before they can receive their first
event.

## Verifying by hand

```
$ hookline verify <secret> <id> <timestamp> <signature> <body>
```

It checks the timestamp first, then the signature, and says which failed. On a
signature mismatch it prints the one it expected, which turns "it does not
work" into a two-line diff.
