package webhook

import (
	"net/http"
	"testing"
	"time"
)

const (
	secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"
	id     = "msg_p5jXN8AQM9LWM0D4loKWxJek"
	ts     = "1614265330"
	body   = `{"test": 2432232314}`
	sig    = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
)

func headers(signature string) http.Header {
	h := http.Header{}
	h.Set("webhook-id", id)
	h.Set("webhook-timestamp", ts)
	h.Set("webhook-signature", signature)
	return h
}

const huge = 1000000 * time.Hour

func TestVector(t *testing.T) {
	if err := Verify(secret, headers(sig), []byte(body), huge); err != nil {
		t.Fatalf("the official vector must verify: %v", err)
	}
	if err := Verify(secret, headers(sig), []byte(`{"test": 2432232315}`), huge); err != ErrNoMatch {
		t.Fatalf("a tampered body must be rejected, got %v", err)
	}
	if err := Verify(secret, headers(sig), []byte(body), time.Second); err != ErrStaleTimestamp {
		t.Fatalf("a stale timestamp must be rejected, got %v", err)
	}
	if err := Verify(secret, headers("v1,AAAA "+sig), []byte(body), huge); err != nil {
		t.Fatalf("a rotation header must verify: %v", err)
	}
}
