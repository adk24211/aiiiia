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
