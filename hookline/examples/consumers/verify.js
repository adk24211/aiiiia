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
