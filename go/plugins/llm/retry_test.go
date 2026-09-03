package llm

import (
	"context"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
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
