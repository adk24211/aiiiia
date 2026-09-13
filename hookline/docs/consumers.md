# Receiving a hookline webhook

Everything a consumer needs is in three headers and the raw body.

```
webhook-id:        msg_06G9HHQQFTNK622Y5F55VEX13W
webhook-timestamp: 1789269571
webhook-signature: v1,x7y65XzTj7GJe4tTgYvc9nCVoWVlsQxooPKv1UniEhE=
content-type:      application/json
```

Verification is the same three steps everywhere:

1. Check `webhook-timestamp` is within a few minutes of now. This is what
   stops a captured request being replayed at you a week later.
2. HMAC-SHA256 the string `{webhook-id}.{webhook-timestamp}.{body}` with the
   base64-decoded secret (drop the `whsec_` prefix).
3. Compare, in constant time, against each space-separated `v1,...` value in
   `webhook-signature`. There is more than one during a secret rotation, and
   any of them matching means the request is authentic.

## The one mistake everybody makes

**The body must be the exact bytes that arrived.** A framework that parses
JSON for you and hands back an object has thrown the bytes away; serialising
that object again produces different bytes — different key order, different
spacing, `1.0` where `1` was — and the signature will not match. Get the raw
body before anything decodes it:

| | |
|---|---|
| Express | `express.raw({ type: "application/json" })` on the route |
| Flask | `request.get_data()` |
| Django | `request.body` |
| Go | `io.ReadAll(r.Body)` before `json.Decode` |
| Rails | `request.raw_post` |
| PHP | `file_get_contents("php://input")` |
| Laravel | `$request->getContent()` |

If it still does not verify, ask the server:

```
hookline verify <secret> <webhook-id> <webhook-timestamp> <webhook-signature> <body>
```

It says which of the two things went wrong, and prints the signature it
expected next to the one you gave it.

## Answering

Answer `2xx` and hookline is done. Answer `4xx` and it stops, because a
request that is wrong will be wrong again; `408` and `429` are the exceptions,
being about timing rather than content. Answer `5xx`, time out, or refuse the
connection and it retries on an exponential schedule with jitter for about
twenty hours.

Answer quickly — the default timeout is fifteen seconds. Do the work
afterwards: acknowledge, then process. A consumer that does five seconds of
work before answering is a consumer that times out the first time you have a
slow day.

**Expect duplicates.** Any at-least-once delivery system has them: an answer
that is lost on the way back looks exactly like a request that never arrived.
`webhook-id` is stable across every retry of the same event, so store it and
ignore an id you have already handled.

---

## Node

```js
const crypto = require("node:crypto");

/**
 * Verify a hookline (Standard Webhooks) signature.
 *
 * `body` must be the raw request bytes. In Express, that means
 * `express.raw({ type: "application/json" })` on this route: once a body
 * parser has turned it into an object, re-serialising it gives different
 * bytes and the signature will not match.
 */
function verify(secret, headers, body, toleranceSeconds = 300) {
  const id = headers["webhook-id"];
  const timestamp = headers["webhook-timestamp"];
  const signature = headers["webhook-signature"];
  if (!id || !timestamp || !signature) throw new Error("missing webhook headers");

  const age = Math.abs(Math.floor(Date.now() / 1000) - Number(timestamp));
  if (!Number.isFinite(age) || age > toleranceSeconds) throw new Error("stale timestamp");

  const key = Buffer.from(secret.replace(/^whsec_/, ""), "base64");
  const signed = `${id}.${timestamp}.${Buffer.from(body).toString("utf8")}`;
  const expected = crypto.createHmac("sha256", key).update(signed).digest();

  // The header may carry several space-separated signatures during a secret
  // rotation. One of them matching is enough.
  for (const part of String(signature).split(" ")) {
    const [version, value] = part.split(",");
    if (version !== "v1" || !value) continue;
    const given = Buffer.from(value, "base64");
    if (given.length === expected.length && crypto.timingSafeEqual(given, expected)) return true;
  }
  throw new Error("no signature matched");
}

module.exports = { verify };
```

## Python

```python
import base64, hashlib, hmac, time


def verify(secret: str, headers, body: bytes, tolerance_seconds: int = 300) -> bool:
    """Verify a hookline (Standard Webhooks) signature.

    `body` must be the raw request bytes. In Flask that is `request.get_data()`
    and in Django `request.body` — not a dict that has been parsed and
    re-serialised, which is different bytes and will not match.
    """
    msg_id = headers["webhook-id"]
    timestamp = headers["webhook-timestamp"]
    signature = headers["webhook-signature"]

    if abs(int(time.time()) - int(timestamp)) > tolerance_seconds:
        raise ValueError("stale timestamp")

    key = base64.b64decode(secret.removeprefix("whsec_"))
    signed = b"%s.%s.%s" % (msg_id.encode(), str(timestamp).encode(), body)
    expected = hmac.new(key, signed, hashlib.sha256).digest()

    # Several space-separated signatures during a rotation; one is enough.
    for part in signature.split(" "):
        version, _, value = part.partition(",")
        if version != "v1" or not value:
            continue
        if hmac.compare_digest(base64.b64decode(value), expected):
            return True
    raise ValueError("no signature matched")
```

## Go

```go
package webhook

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"net/http"
	"strconv"
	"strings"
	"time"
)

var (
	ErrMissingHeaders = errors.New("missing webhook headers")
	ErrStaleTimestamp = errors.New("stale timestamp")
	ErrNoMatch        = errors.New("no signature matched")
)

// Verify checks a hookline (Standard Webhooks) signature.
//
// body must be the raw request bytes, read with io.ReadAll(r.Body) before
// anything decodes them: a struct that has been unmarshalled and marshalled
// again is different bytes and will not match.
func Verify(secret string, headers http.Header, body []byte, tolerance time.Duration) error {
	id := headers.Get("webhook-id")
	timestamp := headers.Get("webhook-timestamp")
	signature := headers.Get("webhook-signature")
	if id == "" || timestamp == "" || signature == "" {
		return ErrMissingHeaders
	}

	seconds, err := strconv.ParseInt(timestamp, 10, 64)
	if err != nil {
		return ErrStaleTimestamp
	}
	if age := time.Since(time.Unix(seconds, 0)); age > tolerance || age < -tolerance {
		return ErrStaleTimestamp
	}

	key, err := base64.StdEncoding.DecodeString(strings.TrimPrefix(secret, "whsec_"))
	if err != nil {
		return err
	}
	mac := hmac.New(sha256.New, key)
	mac.Write([]byte(id + "." + timestamp + "."))
	mac.Write(body)
	expected := mac.Sum(nil)

	// Several space-separated signatures during a rotation; one is enough.
	for _, part := range strings.Split(signature, " ") {
		version, value, found := strings.Cut(part, ",")
		if !found || version != "v1" {
			continue
		}
		given, err := base64.StdEncoding.DecodeString(value)
		if err != nil {
			continue
		}
		if hmac.Equal(given, expected) {
			return nil
		}
	}
	return ErrNoMatch
}
```

## Ruby

```ruby
require "base64"
require "openssl"

module Hookline
  Invalid = Class.new(StandardError)

  # Verify a hookline (Standard Webhooks) signature.
  #
  # +body+ must be the raw request bytes. In Rails that is
  # +request.raw_post+, not +params+: a hash that has been parsed and dumped
  # again is different bytes and will not match.
  def self.verify(secret, headers, body, tolerance_seconds: 300)
    id        = headers["webhook-id"]
    timestamp = headers["webhook-timestamp"]
    signature = headers["webhook-signature"]
    raise Invalid, "missing webhook headers" unless id && timestamp && signature

    raise Invalid, "stale timestamp" if (Time.now.to_i - timestamp.to_i).abs > tolerance_seconds

    key = Base64.decode64(secret.delete_prefix("whsec_"))
    signed = "#{id}.#{timestamp}.#{body}"
    expected = OpenSSL::HMAC.digest("SHA256", key, signed)

    # Several space-separated signatures during a rotation; one is enough.
    signature.split(" ").each do |part|
      version, value = part.split(",", 2)
      next unless version == "v1" && value

      given = Base64.decode64(value)
      return true if given.bytesize == expected.bytesize &&
                     OpenSSL.secure_compare(given, expected)
    end
    raise Invalid, "no signature matched"
  end
end
```

## PHP

```php
<?php

/**
 * Verify a hookline (Standard Webhooks) signature.
 *
 * $body must be the raw request bytes: file_get_contents('php://input'),
 * not $_POST or a decoded array that has been re-encoded, which is different
 * bytes and will not match.
 *
 * @param array<string,string> $headers lower-cased header names
 * @throws RuntimeException when the request is not authentic
 */
function hookline_verify(
    string $secret,
    array $headers,
    string $body,
    int $toleranceSeconds = 300
): bool {
    $id = $headers['webhook-id'] ?? null;
    $timestamp = $headers['webhook-timestamp'] ?? null;
    $signature = $headers['webhook-signature'] ?? null;
    if ($id === null || $timestamp === null || $signature === null) {
        throw new RuntimeException('missing webhook headers');
    }

    if (abs(time() - (int) $timestamp) > $toleranceSeconds) {
        throw new RuntimeException('stale timestamp');
    }

    $key = base64_decode(preg_replace('/^whsec_/', '', $secret), true);
    if ($key === false) {
        throw new RuntimeException('the secret is not base64');
    }
    $expected = hash_hmac('sha256', "{$id}.{$timestamp}.{$body}", $key, true);

    // Several space-separated signatures during a rotation; one is enough.
    foreach (explode(' ', $signature) as $part) {
        $pieces = explode(',', $part, 2);
        if (count($pieces) !== 2 || $pieces[0] !== 'v1') {
            continue;
        }
        $given = base64_decode($pieces[1], true);
        if ($given !== false && hash_equals($expected, $given)) {
            return true;
        }
    }
    throw new RuntimeException('no signature matched');
}
```

---

Every snippet above is run against the Standard Webhooks specification's own
test vector, a tampered body, an expired timestamp and a rotation header
before it is published here. The vector, if you want to check your own:

```
secret    whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw
id        msg_p5jXN8AQM9LWM0D4loKWxJek
timestamp 1614265330
body      {"test": 2432232314}
signature v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=
```
