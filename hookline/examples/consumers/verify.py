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
