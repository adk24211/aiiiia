#!/bin/sh
# Run every consumer snippet against the Standard Webhooks test vector.
#
# These are the snippets published in docs/consumers.md. They are the first
# code a customer of yours ever runs, and a signature example that does not
# verify is worse than none at all, so they are checked rather than written.
#
# Each runtime that is missing is skipped, not failed: this has to be runnable
# on a laptop that has three of the five.
set -e
cd "$(dirname "$0")"
failed=0

run() {
    name=$1
    shift
    if ! command -v "$1" > /dev/null 2>&1; then
        printf '%-8s skipped, no %s\n' "$name" "$1"
        return
    fi
    if output=$("$@" 2>&1); then
        case "$output" in
            *FAIL*) printf '%-8s FAILED\n%s\n' "$name" "$output"; failed=1 ;;
            *) printf '%-8s ok\n' "$name" ;;
        esac
    else
        printf '%-8s FAILED\n%s\n' "$name" "$output"
        failed=1
    fi
}

run node   node check.js
run python python3 check.py
run ruby   ruby check.rb
run php    php check.php
run go     go test ./...

exit $failed
