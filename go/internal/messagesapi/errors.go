package messagesapi

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"

	"github.com/andreylukin/bough/internal/agentllm"
)

// errCut is a stream that ended before message_stop: a dropped
// connection, a proxy timeout, the idle watchdog. The reply is
// incomplete, so it is retried from the start.
var errCut = errors.New("the stream ended before the message was complete")

// errIdle is the idle watchdog closing a silent stream.
var errIdle = errors.New("the stream went silent")

// class is what the adapter does about an error.
type class int

const (
	fatal     class = iota // return it
	rateLimit              // wait as told, many times
	overload               // back off longer, several times
	transient              // back off briefly, a few times
	badBeta                // drop the named beta for the process, retry once
	binding                // strip thinking for this adapter, retry once
	overflow               // wrap agentllm.ErrContextOverflow, return
)

// failure is one classified attempt error.
type failure struct {
	err    error
	class  class
	status int
	after  time.Duration // a wait the server asked for; 0 = use the schedule
	betas  []string      // badBeta: the values the server named
	msg    string        // the API's own message, when it sent one
}

// apiMessage reads error.message out of an API error body.
func apiMessage(e *anthropic.Error) string {
	var env struct {
		Error struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	if json.Unmarshal([]byte(e.RawJSON()), &env) == nil && env.Error.Message != "" {
		return env.Error.Message
	}
	return e.RawJSON()
}

func classify(err error, now time.Time) failure {
	f := failure{err: err}
	var api *anthropic.Error
	if !errors.As(err, &api) {
		if errors.Is(err, errCut) || errors.Is(err, errIdle) || transportRetryable(err) {
			f.class = transient
		}
		return f
	}
	f.status = api.StatusCode
	f.msg = apiMessage(api)
	typ := string(api.Type())
	switch {
	case typ == "rate_limit_error" || api.StatusCode == http.StatusTooManyRequests:
		f.class = rateLimit
		if api.Response != nil {
			f.after = rateLimitWait(api.Response.Header, now)
		}
	// An in-stream overloaded_error arrives on a 200: the status says
	// the request was fine, the type says it was not.
	case typ == "overloaded_error" || api.StatusCode == 529:
		f.class = overload
	case typ == "api_error" || api.StatusCode >= 500 || api.StatusCode == http.StatusRequestTimeout:
		f.class = transient
	case api.StatusCode == http.StatusBadRequest || typ == "invalid_request_error":
		m := strings.ToLower(f.msg)
		switch {
		case strings.Contains(m, "prompt is too long") || strings.Contains(m, "context window") || strings.Contains(m, "input length and `max_tokens` exceed"):
			f.class = overflow
		case strings.Contains(m, "anthropic-beta"):
			f.class = badBeta
			f.betas = namedBetas(f.msg)
		case strings.Contains(m, "bound to a different conversation") || strings.Contains(m, "invalid `signature`") || strings.Contains(m, "invalid signature"):
			f.class = binding
		}
	}
	return f
}

// namedBetas pulls the values out of "Unexpected value(s) `a`, `b` for
// the `anthropic-beta` header."
func namedBetas(msg string) []string {
	head, _, ok := strings.Cut(msg, "for the `anthropic-beta`")
	if !ok {
		return nil
	}
	var out []string
	parts := strings.Split(head, "`")
	for i := 1; i < len(parts); i += 2 {
		for _, v := range strings.Split(parts[i], ",") {
			if v = strings.TrimSpace(v); v != "" {
				out = append(out, v)
			}
		}
	}
	return out
}

func transportRetryable(err error) bool {
	if err == nil || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return false
	}
	if errors.Is(err, io.ErrUnexpectedEOF) || errors.Is(err, io.EOF) {
		return true
	}
	var ne net.Error
	if errors.As(err, &ne) {
		return true
	}
	s := err.Error()
	for _, m := range []string{"connection reset", "broken pipe", "EOF", "TLS handshake", "bad record MAC", "connection refused", "no such host", "http2:", "timeout"} {
		if strings.Contains(s, m) {
			return true
		}
	}
	return false
}

// rateLimitWait is the wait a 429 asks for: retry-after first, then the
// latest of the anthropic-ratelimit-*-reset times that is still ahead.
func rateLimitWait(h http.Header, now time.Time) time.Duration {
	if v := strings.TrimSpace(h.Get("Retry-After")); v != "" {
		if n, err := strconv.ParseFloat(v, 64); err == nil && n >= 0 {
			return time.Duration(n * float64(time.Second))
		}
		if t, err := http.ParseTime(v); err == nil {
			return max(t.Sub(now), 0)
		}
	}
	var wait time.Duration
	for _, k := range []string{"requests", "tokens", "input-tokens", "output-tokens"} {
		v := h.Get("anthropic-ratelimit-" + k + "-reset")
		if v == "" {
			continue
		}
		if t, err := time.Parse(time.RFC3339, v); err == nil && t.Sub(now) > wait {
			wait = t.Sub(now)
		}
	}
	return min(wait, maxRateLimitWait)
}

const maxRateLimitWait = 2 * time.Minute

// Schedules. Rate limits get the long budget the loop's retry.go has
// always used, because a bench trial that dies on one is a wasted
// trial; overloads back off harder than transient failures.
var (
	rateLimitDelays = []time.Duration{5 * time.Second, 10 * time.Second, 20 * time.Second, 30 * time.Second, 60 * time.Second, 90 * time.Second, 120 * time.Second}
	overloadDelays  = []time.Duration{10 * time.Second, 20 * time.Second, 40 * time.Second, 60 * time.Second}
	transientDelays = []time.Duration{time.Second, 2 * time.Second, 4 * time.Second, 8 * time.Second}
)

const (
	rateLimitAttempts = 15
	overloadAttempts  = 8
	transientAttempts = 5
)

func (f failure) budget(maxAttempts int) (attempts int, delays []time.Duration) {
	switch f.class {
	case rateLimit:
		attempts, delays = rateLimitAttempts, rateLimitDelays
	case overload:
		attempts, delays = overloadAttempts, overloadDelays
	case transient:
		attempts, delays = transientAttempts, transientDelays
	default:
		return 1, nil
	}
	if maxAttempts > 0 {
		attempts = maxAttempts
	}
	return attempts, delays
}

// processDropped is the betas the API has refused this process. A
// refused beta stays refused (it is an account property), so every
// later request leaves it out instead of paying a 400 first.
var processDropped sync.Map

func (a *adapter) betaDropped(b anthropic.AnthropicBeta) bool {
	_, ok := a.dropped.Load(string(b))
	return ok
}

// final wraps an error that ends the request, naming the row.
func (f failure) final(model string) error {
	switch f.class {
	case overflow:
		return fmt.Errorf("llm-anthropic: %s: %w", f.msg, agentllm.ErrContextOverflow)
	}
	switch f.status {
	case http.StatusUnauthorized:
		// The same words llm-anthropic uses on the loop (keyhint.go);
		// that package imports this one, so the text is repeated.
		return fmt.Errorf("llm-anthropic: ANTHROPIC_API_KEY was rejected (HTTP 401: %s). Replace it with /connect anthropic <key>, or edit ~/.bough/env", orStatus(f.msg, 401))
	case http.StatusPaymentRequired, http.StatusForbidden, http.StatusNotFound, http.StatusRequestEntityTooLarge:
		return fmt.Errorf("llm-anthropic: model %q: HTTP %d: %s", model, f.status, orStatus(f.msg, f.status))
	}
	if f.status != 0 {
		return fmt.Errorf("llm-anthropic: model %q: HTTP %d: %s", model, f.status, orStatus(f.msg, f.status))
	}
	return fmt.Errorf("llm-anthropic: %w", f.err)
}

func orStatus(msg string, status int) string {
	if msg != "" {
		return msg
	}
	return http.StatusText(status)
}
