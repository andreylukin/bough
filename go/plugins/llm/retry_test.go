package llm

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"github.com/anthropics/anthropic-sdk-go"
)

// Two 503s then a 200: the call succeeds on the third try. A 401 is
// never retried.
func TestOpenrouterRetriesTransientFailures(t *testing.T) {
	retryDelays = []time.Duration{time.Millisecond}
	var n atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if n.Add(1) < 3 {
			w.WriteHeader(503)
			return
		}
		_, _ = w.Write([]byte(`{"choices":[{"message":{"content":"third time"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"cost":0}}`))
	}))
	defer srv.Close()
	o := &openrouterLLM{model: "m", key: "k", endpoint: srv.URL}
	o.once.Do(func() {})
	out, err := o.Complete(context.Background(), "", []Message{{Role: "user", Content: "hi"}})
	if err != nil || out != "third time" || n.Load() != 3 {
		t.Fatalf("out=%q err=%v tries=%d", out, err, n.Load())
	}
	var m atomic.Int32
	bad := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { m.Add(1); w.WriteHeader(401) }))
	defer bad.Close()
	o2 := &openrouterLLM{model: "m", key: "k", endpoint: bad.URL}
	o2.once.Do(func() {})
	if _, err := o2.Complete(context.Background(), "", []Message{{Role: "user", Content: "hi"}}); err == nil || m.Load() != 1 {
		t.Fatalf("401 must fail once, not retry: err=%v tries=%d", err, m.Load())
	}
}

func TestRetryableErrClassifies(t *testing.T) {
	for _, s := range []string{"remote error: tls: bad record MAC", "read: connection reset by peer", "unexpected EOF"} {
		if !retryableErr(errString(s)) {
			t.Fatalf("%q should retry", s)
		}
	}
	if retryableErr(context.Canceled) {
		t.Fatal("cancel must not retry")
	}
}

type errString string

func (e errString) Error() string { return string(e) }

// The anthropic client returns a typed error: its 429/5xx is worth
// another try, its 4xx is not, and a transport error is judged by text.
func TestRetryableTypedError(t *testing.T) {
	cases := []struct {
		err  error
		want bool
	}{
		{&anthropic.Error{StatusCode: 429}, true},
		{&anthropic.Error{StatusCode: 503}, true},
		{&anthropic.Error{StatusCode: 401}, false},
		{&anthropic.Error{StatusCode: 404}, false},
		{errors.New("remote error: tls: bad record MAC"), true},
		{errors.New("model does not exist"), false},
		{context.Canceled, false},
		{nil, false},
	}
	for _, c := range cases {
		if got := retryable(c.err); got != c.want {
			t.Fatalf("retryable(%v) = %v, want %v", c.err, got, c.want)
		}
	}
}

// Cerebras retries a 503 and gives up on a 400, like every other
// provider — a transient failure there used to end the turn.
func TestCerebrasRetriesTransientFailures(t *testing.T) {
	retryDelays = []time.Duration{time.Millisecond}
	t.Setenv("CEREBRAS_API_KEY", "k")

	var hits atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if hits.Add(1) < 3 {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		w.Write([]byte(`{"choices":[{"message":{"role":"assistant","content":"hi"}}]}`))
	}))
	defer srv.Close()
	old := cerebrasURL
	cerebrasURL = srv.URL
	defer func() { cerebrasURL = old }()

	c := &cerebrasLLM{model: "m"}
	got, err := c.Complete(context.Background(), "sys", []Message{{Role: "user", Content: "x"}})
	if err != nil {
		t.Fatalf("Complete: %v", err)
	}
	if got != "hi" || hits.Load() != 3 {
		t.Fatalf("got %q after %d hits", got, hits.Load())
	}

	hits.Store(0)
	bad := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.WriteHeader(http.StatusBadRequest)
		w.Write([]byte(`{"error":{"message":"nope"}}`))
	}))
	defer bad.Close()
	cerebrasURL = bad.URL
	if _, err := (&cerebrasLLM{model: "m"}).Complete(context.Background(), "sys", nil); err == nil {
		t.Fatal("a 400 must fail")
	}
	if hits.Load() != 1 {
		t.Fatalf("a 400 was retried %d times", hits.Load())
	}
}
