# Security

A webhook sender makes HTTP requests to addresses its *users* choose, from
inside your network, holding credentials for other people's systems. That is
an unusual amount of trust for a small service, and it is worth being explicit
about what is defended and what is not.

## Server-side request forgery

This is the one that gets exploited. A customer sets their endpoint URL to
something inside your infrastructure, and you make the request for them.

Refused, by default:

| | |
|---|---|
| `169.254.169.254`, `fd00:ec2::254` | cloud metadata. On a default EC2 or GCE instance this hands out credentials |
| `127.0.0.0/8`, `::1` | loopback: the Redis, Elasticsearch or admin port that is unauthenticated because it only listens locally |
| `::ffff:127.0.0.1` | the same loopback address as IPv4-mapped IPv6, which gets past a check that only knows about dotted quads |
| `10/8`, `172.16/12`, `192.168/16`, `fc00::/7` | private networks |
| `169.254/16`, `fe80::/10` | link-local |
| `100.64/10` | carrier-grade NAT |
| `198.18/15`, `192.0.2/24`, `2001:db8::/32` | benchmarking and documentation ranges |
| `224/4`, `240/4`, `255.255.255.255` | multicast, reserved, broadcast |
| `0.0.0.0`, `::` | this host, by several routes |
| `http://` | plaintext |
| `https://user:pass@host/` | credentials in the URL |
| anything but `http`/`https` | `file:`, `gopher:`, `ftp:` |

Two more subtle ones:

**DNS rebinding.** A name that answers with a public address when the URL is
validated and a private one when the connection is made. Checking the URL is
not enough, and re-resolving at connection time is exactly the hole. hookline
filters inside the resolver the HTTP client uses, so the address that is
checked is the address that is dialled — there is no window between them. Every
address a name answers with is checked, not just the first.

**Redirects.** A public URL that answers `307` to `http://127.0.0.1:6379/`.
Redirects are not followed at all. A redirect is recorded as the failure it is.

Proxy environment variables are ignored on purpose. A proxy resolves names
itself, which would put all of the above out of the loop.

The policy is checked twice: when an endpoint is created, so a bad URL is a
`400` at the moment someone types it, and again at send time, so a policy
tightened after the fact applies to endpoints that already exist.

`HOOKLINE_ALLOW_PRIVATE_DESTINATIONS=1` turns the address checks off. It is
for local development. A deployment whose consumers genuinely are inside the
same network wants it too, and should understand that it is then relying on
network policy rather than on this.

`HOOKLINE_DENIED_HOSTS` refuses hostnames and their subdomains regardless of
where they resolve.

## Credentials

API tokens are 256 bits from the operating system's generator. Only a SHA-256
hash is stored, so a copy of the database is not a copy of the credentials —
which matters, because the database is a file people back up, copy to a laptop
to debug, and occasionally leave somewhere.

The lookup is by hash, so no stored value is ever compared against something
an attacker controls the timing of. Revoked keys are excluded by the query
rather than by a check a caller could forget.

Signing secrets *are* stored in the clear, and have to be: they are used to
sign, not to check something already signed. A database read is therefore a
compromise of every endpoint's secrets, and the response is a rotation, which
is a supported operation rather than an incident.

Scopes exist so that the credential in your application servers — the one in
the most places and the most likely to leak — can post events and nothing
else. Give those `publish`. Keep `admin` for the machine an operator sits at.

## What a consumer must still do

Verify the signature, and check the timestamp *before* the signature. Without
a freshness check a captured request is valid forever. See
[consumers.md](consumers.md).

## Payloads and responses

Outgoing bodies are capped at `HOOKLINE_MAX_PAYLOAD_BYTES` (1 MiB), refused at
the API rather than at send time. Response bodies are streamed and stopped at
`HOOKLINE_MAX_RESPONSE_SNIPPET` (2 KiB), so an endpoint that answers with a
gigabyte costs a couple of kilobytes rather than a gigabyte, and cannot fill
the disk one attempt at a time.

## Tenant isolation

Every endpoint, message and delivery is fetched with the application it must
belong to; the scope is a parameter of the query, not a filter a handler could
forget to add. Crossing a tenant boundary answers `404`, not `403`, because
`403` would confirm the id exists.

## The admin UI

Static markup and the code to call the same API a customer would. It is served
without a credential because it contains nothing: every request it makes
carries a token the operator pastes in, held in that browser's local storage.
It loads nothing from any other origin and says so in a content security
policy. `HOOKLINE_ADMIN_UI=0` removes the route.

## Not defended

* **A malicious operator.** An admin token can read every tenant's payloads.
  There is no defence against your own administrators here and there is not
  meant to be.
* **Traffic analysis.** A consumer can see how often you send them events.
* **Denial of service through the API.** Put a rate limiter in front of it; a
  service doing one thing well should not also be your edge.

## Reporting something

Open an issue for anything that is not exploitable. For anything that is,
please contact the maintainers privately first.
