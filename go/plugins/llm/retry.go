package llm

// Transient transport failures are retried before they become a turn's
// error: a TLS "bad record MAC", a reset connection, an EOF mid-handshake,
// a 429 or a 5xx. Anything else (4xx, a bad key, a cancelled context)
// fails at once.

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"time"
)

const retryAttempts = 3

// retryDelays is a var so tests can shorten it.
var retryDelays = []time.Duration{time.Second, 3 * time.Second}

// retryableErr reports whether a transport error is worth another try.
func retryableErr(err error) bool {
	if err == nil || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return false
	}
	s := err.Error()
	for _, m := range []string{"bad record MAC", "connection reset", "EOF", "broken pipe", "no such host", "timeout", "TLS handshake", "connection refused"} {
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

// withRetries runs do up to retryAttempts times while it reports a
// retryable failure (retry=true), sleeping retryDelays between tries and
// honouring ctx. The last error is returned when every try failed.
func withRetries[T any](ctx context.Context, do func() (T, bool, error)) (T, error) {
	var zero T
	var last error
	for i := 0; i < retryAttempts; i++ {
		v, retry, err := do()
		if err == nil {
			return v, nil
		}
		last = err
		if !retry || i == retryAttempts-1 {
			return zero, err
		}
		d := retryDelays[min(i, len(retryDelays)-1)]
		select {
		case <-time.After(d):
		case <-ctx.Done():
			return zero, ctx.Err()
		}
	}
	return zero, last
}
