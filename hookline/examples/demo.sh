#!/bin/sh
# A whole webhook system on your machine in thirty seconds.
#
# Starts hookline and three consumers — one healthy, one that fails twice then
# recovers, one that is simply down — sends events to all three, and shows what
# each of them did. Everything lives in a scratch directory it deletes on exit.
#
# Needs: a release build (cargo build --release) and python3.
set -e

here=$(cd "$(dirname "$0")/.." && pwd)
bin="$here/target/release/hookline"
[ -x "$bin" ] || { echo "build it first:  cargo build --release"; exit 1; }
command -v python3 > /dev/null || { echo "this demo needs python3 for the consumers"; exit 1; }

work=$(mktemp -d)
port=${HOOKLINE_DEMO_PORT:-8080}
cleanup() {
    [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null
    [ -n "$consumer_pid" ] && kill "$consumer_pid" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }

cat > "$work/consumers.py" <<'PY'
import http.server, socketserver, threading, sys

CALLS = {9201: 0, 9202: 0, 9203: 0}

def make(port):
    class H(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("content-length", 0)))
            CALLS[port] += 1
            # 9201 always works; 9202 fails twice then recovers; 9203 is down.
            if port == 9201 or (port == 9202 and CALLS[port] > 2):
                self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
            else:
                self.send_response(503 if port == 9202 else 500)
                self.end_headers(); self.wfile.write(b"upstream unavailable")
        def log_message(self, *a): pass
    return H

for p in CALLS:
    srv = socketserver.TCPServer(("127.0.0.1", p), make(p))
    srv.allow_reuse_address = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
threading.Event().wait()
PY

# curl exits non-zero when nothing is listening, which is the portable way to
# ask; /dev/tcp is a bash-ism and this is a /bin/sh script.
listening() { curl -s --noproxy '*' -m 1 -o /dev/null "http://127.0.0.1:$1/"; }

for p in 9201 9202 9203; do
    if listening "$p"; then
        echo "port $p is already in use; stop whatever is on it and try again"
        exit 1
    fi
done

python3 "$work/consumers.py" &
consumer_pid=$!
for _ in $(seq 1 40); do
    listening 9203 && break
    sleep 0.25
done
listening 9203 || { echo "the demo consumers did not start"; exit 1; }

export HOOKLINE_DATABASE="$work/demo.db"
export HOOKLINE_LISTEN="127.0.0.1:$port"
# Loopback consumers are exactly what the default policy exists to refuse.
# This is a demo on one machine; leave both off in production.
export HOOKLINE_ALLOW_PRIVATE_DESTINATIONS=1
export HOOKLINE_ALLOW_HTTP=1
export HOOKLINE_BREAKER_FAILURES=3
# A one-second base rather than the five-second default, so the flaky
# endpoint's recovery happens while you are still looking at it.
export HOOKLINE_RETRY_BASE_SECS=1
export HOOKLINE_LOG=error

token=$("$bin" key create demo admin 2>/dev/null)
"$bin" serve &
server_pid=$!

for _ in $(seq 1 50); do
    "$bin" health > /dev/null 2>&1 && break
    sleep 0.2
done

api="http://127.0.0.1:$port/v1"
post() { curl -s --noproxy '*' -H "authorization: Bearer $token" -H 'content-type: application/json' "$@"; }
field() { python3 -c "import sys,json; print(json.load(sys.stdin)['$1'])"; }

say "1. a tenant, addressed by your own customer id"
post -d '{"name":"Acme Payments","uid":"customer-42"}' "$api/apps" > /dev/null
echo "   customer-42"

say "2. three endpoints: one healthy, one flaky, one down"
for p in 9201 9202 9203; do
    secret=$(post -d "{\"url\":\"http://127.0.0.1:$p/hook\"}" "$api/apps/customer-42/endpoints" | field secret)
    echo "   127.0.0.1:$p   $secret"
done

say "3. send an event"
post -d '{"event_type":"invoice.paid","payload":{"invoice_id":"inv_1","amount":4200,"currency":"KRW"}}' \
     "$api/apps/customer-42/messages" | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("  ", d["id"], "queued to", len(d["deliveries"]), "endpoints")'

printf '\n   waiting for the retries to play out'
for _ in $(seq 1 12); do printf '.'; sleep 1; done
printf '\n'

say "4. what happened"
TOKEN="$token" API="$api" python3 <<'REPORT'
import json, os, subprocess

token, api = os.environ["TOKEN"], os.environ["API"]

def get(path):
    out = subprocess.run(
        ["curl", "-s", "--noproxy", "*", "-H", "authorization: Bearer " + token, api + path],
        capture_output=True, text=True).stdout
    return json.loads(out)

for ep in get("/apps/customer-42/endpoints?limit=50")["data"]:
    health = get("/apps/customer-42/endpoints/%s/health" % ep["id"])
    circuit = "  CIRCUIT OPEN" if health.get("circuit_open_until") else ""
    for d in get("/apps/customer-42/endpoints/%s/deliveries?limit=50" % ep["id"])["data"]:
        attempts = get("/apps/customer-42/deliveries/%s/attempts" % d["id"])
        trail = " -> ".join(
            "%s%s" % (a["status"], " " + str(a["status_code"]) if a.get("status_code") else "")
            for a in attempts)
        print("   %-11s %-10s %s%s" % (ep["url"].replace("http://127.0.0.1:", ":"),
                                       d["status"], trail or "not attempted yet", circuit))
REPORT

say "5. the signature, checked the way a consumer would"
echo "   hookline verify <secret> <webhook-id> <webhook-timestamp> <signature> <body>"
echo "   (and docs/consumers.md has the five lines for Node, Python, Go, Ruby and PHP)"

say "the admin UI is at http://127.0.0.1:$port"
echo "paste this token into it:"
echo
echo "   $token"
echo
echo "Ctrl-C to stop. Everything is deleted on exit."
wait "$server_pid"
