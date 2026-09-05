package llm

// The stall guard bounds SILENCE, not duration: a long reply that keeps
// producing tokens must never be cut, and a stream that goes quiet must
// not hang the turn forever. A real run sat for eight minutes with no
// output and no error before this existed.

import (
	"io"
	"os"
	"strings"
	"testing"
	"time"
)

// slowBody yields each chunk after a delay, then blocks forever unless
// closed — a stream that dies mid-flight.
type slowBody struct {
	chunks  []string
	delay   time.Duration
	i       int
	closed  chan struct{}
	hangEnd bool
}

func newSlowBody(delay time.Duration, hangEnd bool, chunks ...string) *slowBody {
	return &slowBody{chunks: chunks, delay: delay, closed: make(chan struct{}), hangEnd: hangEnd}
}

func (s *slowBody) Read(p []byte) (int, error) {
	if s.i >= len(s.chunks) {
		if !s.hangEnd {
			return 0, io.EOF
		}
		<-s.closed // the far end is gone
		return 0, io.ErrClosedPipe
	}
	select {
	case <-time.After(s.delay):
	case <-s.closed:
		return 0, io.ErrClosedPipe
	}
	n := copy(p, s.chunks[s.i])
	s.i++
	return n, nil
}

func (s *slowBody) Close() error {
	select {
	case <-s.closed:
	default:
		close(s.closed)
	}
	return nil
}

// A stream that keeps producing is never cut, however long it runs in
// total — each chunk resets the clock.
func TestStallGuardAllowsSlowButLiveStream(t *testing.T) {
	body := newSlowBody(30*time.Millisecond, false, "a", "b", "c", "d", "e")
	r := guardStalls(body, 200*time.Millisecond) // total run exceeds the limit
	got, err := io.ReadAll(r)
	if err != nil {
		t.Fatalf("a live stream should read cleanly: %v", err)
	}
	if string(got) != "abcde" {
		t.Errorf("got %q, want abcde", got)
	}
}

// A stream that goes quiet fails instead of blocking forever.
func TestStallGuardTripsOnSilence(t *testing.T) {
	body := newSlowBody(10*time.Millisecond, true, "hello")
	r := guardStalls(body, 120*time.Millisecond)
	done := make(chan error, 1)
	go func() {
		_, err := io.ReadAll(r)
		done <- err
	}()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("a stalled stream should fail, not return cleanly")
		}
	case <-time.After(3 * time.Second):
		t.Fatal("the guard did not trip: the read is still blocked")
	}
}

// Close is safe to call after the guard already tripped, and twice.
func TestStallGuardCloseIsIdempotent(t *testing.T) {
	body := newSlowBody(time.Millisecond, true, "x")
	r := guardStalls(body, 10*time.Millisecond)
	buf := make([]byte, 8)
	_, _ = r.Read(buf)
	time.Sleep(60 * time.Millisecond) // let it trip
	if err := r.Close(); err != nil {
		t.Errorf("close after trip: %v", err)
	}
	if err := r.Close(); err != nil {
		t.Errorf("second close: %v", err)
	}
}

// The shared client bounds silence but sets no overall deadline: a long
// reply legitimately streams for minutes.
func TestSharedClientHasNoOverallTimeout(t *testing.T) {
	if httpClient.Timeout != 0 {
		t.Errorf("Client.Timeout would cut long streams, got %v", httpClient.Timeout)
	}
	if HTTPClient() != httpClient {
		t.Error("HTTPClient should expose the shared client")
	}
}

// Every provider that speaks HTTP directly uses the shared client, so
// none of them can go back to http.DefaultClient by accident.
func TestNoProviderUsesDefaultClient(t *testing.T) {
	for _, f := range []string{"openrouter.go", "openai.go", "cerebras.go"} {
		b, err := readSource(f)
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(b, "http.DefaultClient") {
			t.Errorf("%s uses http.DefaultClient, which has no timeout at all", f)
		}
	}
}

func readSource(name string) (string, error) {
	b, err := os.ReadFile(name)
	return string(b), err
}
