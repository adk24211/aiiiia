# API

Every route is under `/v1`, takes `authorization: Bearer <token>`, and answers
JSON. Timestamps are milliseconds since the Unix epoch.

Two routes are unauthenticated: `GET /health` and `GET /version`. A health
check that needs a credential is one a load balancer cannot make.

## Errors

```json
{ "error": { "code": "not_found", "message": "no such endpoint" } }
```

Branch on `code`, not on `message`; the messages are free to improve.

| Status | Code | |
|---|---|---|
| 400 | `invalid_request` | the request is malformed or asks for something impossible |
| 401 | `unauthorized` | no token, an unknown token, or a revoked one |
| 403 | `forbidden` | a valid token whose scope does not allow this |
| 404 | `not_found` | including anything belonging to another application |
| 409 | `conflict` | a uid already taken, a delivery already finished |
| 429 | `rate_limited` | with a `retry-after` header |
| 500 | `storage_error` | transient; retry |

## Paging

Listings answer `{ "data": [...], "next_cursor": "...", "has_more": true }`.
Pass `?cursor=` and `?limit=` (default 50, maximum 250). Cursors are ids, not
offsets, so a page boundary does not move when a row is inserted.

## Scopes

| Scope | |
|---|---|
| `admin` | everything, including minting and revoking keys |
| `publish` | post messages, and nothing else — the scope for your application servers |
| `read` | listings and history, no writes |

## Applications

An application is a tenant. Everything else belongs to one. Give it a `uid` —
your own identifier for that customer — and it works in place of the hookline
id in every route below, so you need no mapping table.

| | |
|---|---|
| `POST /v1/apps` | `{ name, uid?, metadata? }` |
| `GET /v1/apps` | |
| `GET /v1/apps/{app}` | by id or uid |
| `PATCH /v1/apps/{app}` | `{ name?, uid?, metadata? }` |
| `DELETE /v1/apps/{app}?confirm_name={name}` | destroys its endpoints, messages and history |

The delete wants the application's name repeated back. It is the only route
that can destroy history, and a script with the wrong id in a variable is a
thing that happens.

## Endpoints

| | |
|---|---|
| `POST /v1/apps/{app}/endpoints` | `{ url, description?, event_types?, rate_limit?, secret? }` |
| `GET /v1/apps/{app}/endpoints` | |
| `GET /v1/apps/{app}/endpoints/{endpoint}` | |
| `PATCH /v1/apps/{app}/endpoints/{endpoint}` | |
| `DELETE /v1/apps/{app}/endpoints/{endpoint}` | |
| `POST /v1/apps/{app}/endpoints/{endpoint}/disable` | `{ reason? }`, and cancels what is queued for it |
| `POST /v1/apps/{app}/endpoints/{endpoint}/enable` | and resumes it |
| `POST /v1/apps/{app}/endpoints/{endpoint}/resume` | the endpoint works again: clear the breaker and make queued deliveries due now |
| `GET /v1/apps/{app}/endpoints/{endpoint}/health` | the breaker's view |

The create returns the signing secret. It is also available from the secrets
route; it is not in any listing.

`event_types` is a list of patterns, or absent for every event. A trailing `*`
matches a prefix: `invoice.*` covers `invoice.paid`. A star anywhere else is
refused, because it reads as a wildcard and would not be one. Event types are
text, not ASCII: `결제.*` and `请求.created` are ordinary patterns.

`event_types` distinguishes absent from null in a `PATCH`: omitting it leaves
the filter alone, and sending `null` clears it.

`rate_limit` is deliveries per minute to that endpoint, at least 1, or absent
for no limit of its own. Zero is refused: it would park every delivery for
ever, and stopping delivery to an endpoint is `disable`, which says so in the
endpoint's own state. It is enforced as spacing rather than a bucket — at 60 a minute,
one a second — so there is no burst to absorb and the rate is exact. A limited
endpoint with a backlog does not hold up anyone else's deliveries; its next
allowed time is in its `health`.

### Secrets

| | |
|---|---|
| `GET /v1/apps/{app}/endpoints/{endpoint}/secrets` | |
| `POST /v1/apps/{app}/endpoints/{endpoint}/secrets/rotate` | `{ secret?, grace_secs? }` |
| `DELETE /v1/apps/{app}/endpoints/{endpoint}/secrets/{secret}` | expire one immediately |

See [signatures.md](signatures.md). The last active secret cannot be revoked.

## Messages

```http
POST /v1/apps/{app}/messages

{ "event_type": "invoice.paid",
  "payload": { "amount": 4200 },
  "idempotency_key": "charge-42" }
```

Answers `202` with the message and one delivery per matching endpoint. An
empty `deliveries` means no endpoint subscribes to this event type, which is
worth noticing in your own code.

Post the same `idempotency_key` again and you get `200`, the original message,
and `"duplicate": true`; nothing new is queued. The key may also be sent as an
`idempotency-key` header. The first payload wins — a retry is the same
request, and letting a different body through would be a silent rewrite.

| | |
|---|---|
| `GET /v1/apps/{app}/messages` | `?event_type=` |
| `GET /v1/apps/{app}/messages/{message}` | |
| `GET /v1/apps/{app}/messages/{message}/deliveries` | |

## Deliveries and attempts

A delivery is one (message, endpoint) pair: the thing that is retried,
succeeds, or is given up on. An attempt is one HTTP request.

| | |
|---|---|
| `GET /v1/apps/{app}/endpoints/{endpoint}/deliveries` | `?status=pending\|succeeded\|failed\|cancelled` |
| `GET /v1/apps/{app}/endpoints/{endpoint}/attempts` | `?status=success\|failure` |
| `GET /v1/apps/{app}/deliveries/{delivery}` | |
| `GET /v1/apps/{app}/deliveries/{delivery}/attempts` | |
| `POST /v1/apps/{app}/deliveries/{delivery}/replay` | queue it again from attempt zero |
| `POST /v1/apps/{app}/deliveries/{delivery}/cancel` | stop retrying |

### Bulk replay

```http
POST /v1/apps/{app}/endpoints/{endpoint}/replay

{ "since": 1789000000000, "status": "failed", "limit": 250 }
```

Answers `{ "replayed": 250, "more": true }`. Bounded and repeatable rather
than one sweeping call: an endpoint with a month of failures behind it would
otherwise receive all of them the moment it came back.

A bulk replay also *resumes* the endpoint: it clears the breaker and makes
everything still queued for that endpoint due now. Clearing the breaker alone
is not enough, and this is the part that is easy to get wrong. When the
breaker opened, each failing delivery's next attempt was pushed out to
whichever came later — its own backoff, or the end of the cooldown — and after
a long outage that cooldown is half an hour. An operator who has just fixed
their endpoint would otherwise watch a queue that is no longer blocked deliver
nothing for another half hour.

`POST .../resume` is the same thing without re-queueing anything terminal: use
it when the failures have not been given up on yet, which is the usual case
while a breaker is open. It answers `brought_forward` — how many deliveries it
un-parked.

## Keys

| | |
|---|---|
| `POST /v1/keys` | `{ name, scope? }` — scope defaults to `publish` |
| `GET /v1/keys` | |
| `DELETE /v1/keys/{key}` | |

The token is in the create response and nowhere else; what is stored is a
SHA-256 hash. The first key is minted from the command line:

```
hookline key create first-admin admin
```

## Stats

`GET /v1/stats` — queue depth, how long the oldest due delivery has been
waiting, endpoints and open circuits, and the last hour's attempts and
successes.

`oldest_due_age_ms` is the number to alert on. A queue that is deep but moving
is fine; a queue that is shallow but stuck is not.
