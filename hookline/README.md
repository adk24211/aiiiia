# hookline

Reliable webhook delivery, as a single binary you run yourself.

Sending a webhook is one line of code. Sending it *reliably* is a durable
queue, a retry schedule, a signature scheme, secret rotation, a circuit
breaker, an audit trail and a way to replay — and every team that ships
webhooks writes a worse version of all seven, usually twice.

```
$ hookline serve
listening address=0.0.0.0:8080
```

```http
POST /v1/apps/customer-42/messages
authorization: Bearer hl_...

{ "event_type": "invoice.paid", "payload": { "amount": 4200 } }
```

Your customer's endpoint receives that payload, signed, with retries if it
does not answer, and you can see every attempt it took.

* **One binary, one file.** SQLite in WAL mode. No Postgres, no Redis, no
  broker. `docker run` or `./hookline` and it is up.
* **Standard Webhooks signatures.** The same `webhook-id` /
  `webhook-timestamp` / `webhook-signature` headers Stripe-style consumers
  already know, verified here against the specification's own test vector.
* **Rotation without an outage.** Requests are signed with every active
  secret, so a consumer moves to the new one on their own schedule.
* **SSRF closed properly.** Your users choose the URLs. Cloud metadata,
  loopback, private ranges, IPv4-mapped IPv6, DNS rebinding and redirects are
  all refused — and it is the resolver itself that refuses, so the address
  checked is the address dialled.
* **Replay.** A consumer that was down for an hour asks for the hour back,
  and you have it.
* **An audit trail you can answer support questions from.** Every request,
  its status, its timing, and the first two kilobytes of what came back.

## Try it

```bash
cargo build --release
export HOOKLINE_DATABASE=./hookline.db

# Mint a credential. This is the only time the token exists.
./target/release/hookline key create local admin
./target/release/hookline serve
```

Open <http://localhost:8080> for the admin UI, paste the token, and send a
test event. Or from the shell:

```bash
TOKEN=hl_...
API=http://localhost:8080/v1

curl -s $API/apps -H "authorization: Bearer $TOKEN" \
  -d '{"name":"Acme","uid":"customer-42"}'

curl -s $API/apps/customer-42/endpoints -H "authorization: Bearer $TOKEN" \
  -d '{"url":"https://consumer.example.com/hooks","event_types":["invoice.*"]}'
# -> { ..., "secret": "whsec_..." }   give this to the consumer

curl -s $API/apps/customer-42/messages -H "authorization: Bearer $TOKEN" \
  -d '{"event_type":"invoice.paid","payload":{"amount":4200}}'
```

## Receiving

Your customer verifies the signature. It is five lines in any language, and
[`docs/consumers.md`](docs/consumers.md) has them for Node, Python, Go, Ruby
and PHP — each one checked against the same vector the server is.

```js
const signed = `${id}.${timestamp}.${rawBody}`;
const expected = crypto.createHmac("sha256", secretBytes).update(signed).digest("base64");
```

When it does not verify, ask the server why:

```
$ hookline verify whsec_... msg_... 1739...  'v1,abc...' '{"amount":4200}'
the signature does not match.
  given    v1,abc...
  expected v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=

The signed string is `{id}.{timestamp}.{body}`, and the body must be the exact
bytes that were sent: a body that has been parsed and re-serialised is a
different body.
```

That last sentence is the answer roughly four times out of five.

## Documentation

| | |
|---|---|
| [`docs/api.md`](docs/api.md) | every route, with what it answers |
| [`docs/signatures.md`](docs/signatures.md) | the signature scheme and rotation |
| [`docs/consumers.md`](docs/consumers.md) | verification code in five languages |
| [`docs/deploy.md`](docs/deploy.md) | Docker, systemd, backups, tuning |
| [`docs/design.md`](docs/design.md) | why it is built this way |
| [`docs/security.md`](docs/security.md) | the threat model and what is refused |

## Configuration

Everything has a default that works; every default is overridable from the
environment. `hookline help` lists them. The two worth knowing:

| Variable | Default | |
|---|---|---|
| `HOOKLINE_DATABASE` | `hookline.db` | the only state there is |
| `HOOKLINE_ALLOW_PRIVATE_DESTINATIONS` | off | turn on **only** for local development |

## Status

The delivery path — queue, retries, signatures, breaker, replay, the audit
trail, SSRF — is covered by tests that run the real server against a real
consumer over real HTTP. Three bugs in this repository's history were found
by exactly those tests and are pinned by them:

* a transaction that read before it wrote failed with `SQLITE_BUSY` on the
  upgrade, which the busy timeout cannot wait out;
* `?limit=` was silently rejected on any listing whose paging struct sat
  behind a filter;
* an explicit replay was accepted, reported what it queued, and delivered
  nothing while the circuit breaker's cooldown ran.

## Licence

MIT. See [`LICENSE`](LICENSE), and [`docs/licensing.md`](docs/licensing.md) if
you are considering a different arrangement for a hosted version.
