# Deploying

One process, one file, no other services. The whole state of hookline is the
SQLite database; back that up and you have backed up everything.

## Docker

```bash
docker compose up -d --build
docker compose exec hookline hookline key create admin admin
```

The image runs as an unprivileged user and keeps its database on a volume at
`/data`. Nothing else is written.

## A binary and systemd

```bash
cargo build --release
install -m755 target/release/hookline /usr/local/bin/
useradd --system --home /var/lib/hookline hookline
install -d -o hookline -g hookline /var/lib/hookline
```

```ini
# /etc/systemd/system/hookline.service
[Unit]
Description=hookline
After=network-online.target
Wants=network-online.target

[Service]
User=hookline
Group=hookline
Environment=HOOKLINE_DATABASE=/var/lib/hookline/hookline.db
Environment=HOOKLINE_LISTEN=127.0.0.1:8080
ExecStart=/usr/local/bin/hookline serve
Restart=always
RestartSec=2

# It reads one directory, opens outbound sockets, and needs nothing else.
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ReadWritePaths=/var/lib/hookline
RestrictAddressFamilies=AF_INET AF_INET6
LockPersonality=yes
MemoryDenyWriteExecute=yes

[Install]
WantedBy=multi-user.target
```

Put a TLS terminator in front of it and forward to `127.0.0.1:8080`. hookline
does not terminate TLS itself: every deployment already has something that
does, and a second certificate story is a second thing to renew.

## Restarting

`SIGTERM` or `SIGINT` stops the listener, then waits for the deliveries
already in flight. Nothing is lost either way — an abandoned delivery is one
whose lease expires and is picked up again — but waiting a few seconds costs
nothing and saves a minute of a stalled queue.

Leases held by a process that was killed are cleared when the replacement
starts, which is safe precisely because the process that held them is gone.

## Backups

```bash
sqlite3 /var/lib/hookline/hookline.db ".backup '/backups/hookline-$(date +%F).db'"
```

`.backup` is safe on a live database; copying the file is not, because WAL
means the file alone is not a consistent snapshot. Restoring is putting the
file back and starting the process.

The attempt log is the only table that grows without bound. It is pruned
hourly to `HOOKLINE_ATTEMPT_RETENTION_DAYS` (30 by default; `0` keeps
everything).

## Tuning

| | Default | |
|---|---|---|
| `HOOKLINE_CONCURRENCY` | 32 | deliveries in flight. Raise it if `oldest_due_age_ms` grows while attempts are fast |
| `HOOKLINE_POOL_SIZE` | 8 | database connections. Rarely the bottleneck |
| `HOOKLINE_BATCH_SIZE` | 16 | deliveries claimed per pass |
| `HOOKLINE_REQUEST_TIMEOUT_SECS` | 15 | per attempt |
| `HOOKLINE_LEASE_SECS` | 60 | must outlast the timeout, or a delivery still in flight is handed to a second worker and sent twice. The server refuses to start otherwise |
| `HOOKLINE_MAX_ATTEMPTS` | 10 | about twenty hours with the default schedule |

One process per database. SQLite handles concurrent readers across processes
happily, but two hookline processes on one file would both be claiming from
the same queue, and the lease is the only thing keeping them apart — it would
work, and it would also mean two sets of workers competing for one write lock.
Scale with `HOOKLINE_CONCURRENCY` first; it is the HTTP requests that take the
time, not the database.

## Watching it

`GET /health` for liveness. `GET /v1/stats` for everything else:

* `queue.oldest_due_age_ms` — the one to alert on. Deep but moving is fine;
  shallow but stuck is not.
* `circuits_open` — how many endpoints are being skipped.
* `successes_last_hour / attempts_last_hour` — the ratio to graph.

Logs are structured; set `HOOKLINE_LOG=debug` for per-attempt detail.

## The destination policy

The default refuses plaintext HTTP and every address that is not on the public
internet. That is right for a service whose users choose the URLs, and wrong
for local development:

```
HOOKLINE_ALLOW_PRIVATE_DESTINATIONS=1   # local development only
HOOKLINE_ALLOW_HTTP=1
```

Leave both off in production. See [security.md](security.md) for what they
turn off and why it matters.
