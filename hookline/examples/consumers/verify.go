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
