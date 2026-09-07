package llm

// Transient transport failures are retried before they become a turn's
// error: a TLS "bad record MAC", a reset connection, an EOF mid-handshake,
// a 429 or a 5xx. Anything else (4xx, a bad key, a cancelled context)
// fails at once.

import (
	"context"
	"errors"
	"net/http"

	"github.com/anthropics/anthropic-sdk-go"
	"strings"
	"time"
)

const retryAttempts = 3

// rateLimitAttempts and rateLimitDelays are the budget for a provider
// that says come back later: a bench trial that dies on one of these
// is a wasted trial, so wait it out.
const rateLimitAttempts = 8

var rateLimitDelays = []time.Duration{5 * time.Second, 10 * time.Second, 20 * time.Second, 30 * time.Second, 60 * time.Second}

// malformedAttempts is the budget for a provider rejecting the model's
// own reply as a malformed function call (Gemini): each attempt is an
// independent draw that fails perhaps half the time, and the failed
// attempt cost only the thinking, so it is worth six.
const malformedAttempts = 6

// retryDelays is a var so tests can shorten it.
var retryDelays = []time.Duration{time.Second, 3 * time.Second}

// retryableErr reports whether a transport error is worth another try.
func retryableErr(err error) bool {
	if err == nil || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return false
	}
	s := err.Error()
	for _, m := range []string{"bad record MAC", "connection reset", "EOF", "broken pipe", "no such host", "timeout", "TLS handshake", "connection refused", "provider error mid-stream", "rate-limited", "rate limited", "overloaded", "temporarily"} {
		if strings.Contains(s, m) {
			return true
		}
	}
	return false
}

// retryableStatus reports whether an HTTP status is worth another try.
func retryableStatus(code int) bool {
	return code == http.StatusTooManyRequests || code >= 500
}

// retryable is retryableErr plus the status of a typed SDK error: the
// anthropic client returns *anthropic.Error, whose 429/5xx is worth
// another try but whose 400/401/404 is not.
func retryable(err error) bool {
	if err == nil {
		return false
	}
	if apiErr, ok := errors.AsType[*anthropic.Error](err); ok {
		return retryableStatus(apiErr.StatusCode)
	}
	return retryableErr(err)
}

// withRetries runs do up to retryAttempts times while it reports a
// retryable failure (retry=true), sleeping retryDelays between tries and
// honouring ctx. The last error is returned when every try failed.
func withRetries[T any](ctx context.Context, do func() (T, bool, error)) (T, error) {
	var zero T
	var last error
	attempts := retryAttempts
	for i := 0; i < attempts; i++ {
		v, retry, err := do()
		if err == nil {
			return v, nil
		}
		last = err
		delays := retryDelays
		switch msg := err.Error(); {
		case strings.Contains(msg, "MALFORMED_FUNCTION_CALL"):
			attempts = malformedAttempts
		case strings.Contains(msg, "rate-limited") || strings.Contains(msg, "rate limited") || strings.Contains(msg, "overloaded"):
			attempts = rateLimitAttempts
			delays = rateLimitDelays
		}
		if !retry || i == attempts-1 {
			return zero, err
		}
		d := delays[min(i, len(delays)-1)]
		select {
		case <-time.After(d):
		case <-ctx.Done():
			return zero, ctx.Err()
		}
	}
	return zero, last
}
