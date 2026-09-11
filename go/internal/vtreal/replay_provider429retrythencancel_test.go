package vtreal

// A provider that answers 429 (Retry-After: 2) sends the turn into
// llm's retry backoff; esc during that backoff must end the turn as
// cancelled — not as an error — leave no retry chip and no late delta
// behind, and the next input must reach the provider at once.
//
// The seam is the real llm-openai row pointed at an httptest server
// in this process: the replay model has no retry path of its own
// (withRetries lives in the provider plugins), so a recorded tape
// cannot exercise the backoff. codemode is still the replay row.

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// provider429Server fails every call with 429 until answer is called,
// then streams the reply as a Responses SSE stream. It logs when each
// call came in.
type provider429Server struct {
	*httptest.Server
	mu    sync.Mutex
	ok    bool
	reply string
	calls []time.Time
}

func provider429NewServer(t *testing.T) *provider429Server {
	t.Helper()
	s := &provider429Server{}
	s.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s.mu.Lock()
		s.calls = append(s.calls, time.Now())
		ok, reply := s.ok, s.reply
		s.mu.Unlock()
		if !ok {
			w.Header().Set("Content-Type", "application/json")
			w.Header().Set("Retry-After", "2")
			w.WriteHeader(http.StatusTooManyRequests)
			fmt.Fprint(w, `{"error":{"message":"rate limited, retry after 2s"}}`)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprintf(w, "data: {\"type\":\"response.output_text.delta\",\"delta\":%q}\n\n", reply)
		fmt.Fprintf(w, "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":%q}]}]}}\n\n", reply)
	}))
	t.Cleanup(s.Close)
	return s
}

func (s *provider429Server) answer(reply string) {
	s.mu.Lock()
	s.ok, s.reply = true, reply
	s.mu.Unlock()
}

func (s *provider429Server) times() []time.Time {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]time.Time(nil), s.calls...)
}

// provider429Start is startCfg with llm-openai on srv and a fake key
// in the child's environment only (t.Setenv is off under t.Parallel).
func provider429Start(t *testing.T, srv *provider429Server) *app {
	t.Helper()
	home := t.TempDir()
	tape, err := filepath.Abs(filepath.Join("testdata", "replay", "errors-endtape.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	yml := strings.Replace(replayConfig(tape),
		fmt.Sprintf("- id: llm\n  plugin: replay\n  config: {file: %q}", tape),
		fmt.Sprintf("- id: llm\n  plugin: llm-openai\n  config: {model: gpt-5, base_url: %q}", srv.URL), 1)
	if !strings.Contains(yml, "llm-openai") {
		t.Fatal("provider429Start: replayConfig's llm row changed shape")
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "OPENAI_API_KEY=sk-test",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// provider429Kinds is the kind of every history entry this run wrote.
func provider429Kinds(a *app) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var kinds []string
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			kinds = append(kinds, e.Kind)
		}
	}
	return kinds
}

// provider429Chip is anything the screen could show for a retry in
// flight.
var provider429Chip = regexp.MustCompile(`(?i)retry|rate.limit|429`)

// provider429Backoff sends one input and waits for the first 429, so
// the turn is sleeping in withRetries' backoff when this returns.
func provider429Backoff(t *testing.T, a *app, srv *provider429Server) time.Time {
	t.Helper()
	a.typeText("hello provider")
	a.key(uv.KeyEnter, 0)
	deadline := time.Now().Add(30 * time.Second)
	for len(srv.times()) == 0 {
		if time.Now().After(deadline) {
			t.Fatalf("the provider was never called:\n%s", a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
	return srv.times()[0]
}

// provider429LastRows is the bottom n rows of a screen: the status bar.
func provider429LastRows(s string, n int) string {
	ls := strings.Split(s, "\n")
	return strings.Join(ls[max(0, len(ls)-n):], "\n")
}

func TestProvider429RetryThenCancel(t *testing.T) {
	t.Parallel()

	t.Run("esc_in_backoff_cancels", func(t *testing.T) {
		t.Parallel()
		srv := provider429NewServer(t)
		a := provider429Start(t, srv)
		first := provider429Backoff(t, a, srv)
		time.Sleep(300 * time.Millisecond) // well inside the 5s backoff
		a.key(uv.KeyEsc, 0)
		a.waitFor("■ cancelled")
		if !a.waitDone(1, 10*time.Second) {
			t.Fatalf("the turn never ended after esc:\n%s", a.text())
		}
		if since := time.Since(first); since > 4*time.Second {
			t.Errorf("esc took %v to end the turn: the backoff was not interrupted", since)
		}
		// The loop always notes "done" after the "cancelled" marker
		// (plugins/loop cancel_test); what must not be there is an
		// error, or an assistant entry from a reply that landed late.
		kinds := provider429Kinds(a)
		if !strings.Contains(strings.Join(kinds, " "), "cancelled") {
			t.Errorf("no cancelled marker in history: %v", kinds)
		}
		for _, k := range kinds {
			if k == "error" || k == "assistant" {
				t.Errorf("a cancelled backoff recorded %q: %v", k, kinds)
			}
		}
		// Past the 5s backoff: a retry still scheduled would fire now.
		time.Sleep(6 * time.Second)
		if n := len(srv.times()); n != 1 {
			t.Errorf("the provider was called %d times; a retry outlived the cancel", n)
		}
		s := a.settled()
		if provider429Chip.MatchString(provider429LastRows(s, 3)) {
			t.Errorf("a retry chip is still up after the cancel:\n%s", s)
		}
		if strings.Contains(s, "✗") {
			t.Errorf("the cancelled turn drew an error block:\n%s", s)
		}
		a.check("after cancel")

		srv.answer("SECONDREPLY arrived")
		sent := time.Now()
		a.typeText("try again")
		a.key(uv.KeyEnter, 0)
		a.waitFor("SECONDREPLY")
		// doneCount is 2 already (cancelled + done); this turn adds a done.
		if !a.waitDone(3, 30*time.Second) {
			t.Fatalf("the next turn never finished:\n%s", a.text())
		}
		if d := time.Since(sent); d > 4*time.Second {
			t.Errorf("the next turn took %v: it waited out a stale backoff", d)
		}
		if n := len(srv.times()); n != 2 {
			t.Errorf("want one call for the next turn, provider saw %d in total", n)
		}
		a.check("next turn")
	})

	t.Run("retry_chip_during_backoff", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_PROVIDER429RETRYTHENCANCEL") == "" {
			t.Skip("known bug: nothing on screen says a 429 backoff is in progress — withRetries (plugins/llm/retry.go) sleeps silently and plugins/ui has no retry chip; set BOUGH_KNOWN_PROVIDER429RETRYTHENCANCEL=1 to run")
		}
		t.Parallel()
		srv := provider429NewServer(t)
		a := provider429Start(t, srv)
		provider429Backoff(t, a, srv)
		time.Sleep(500 * time.Millisecond)
		if s := a.settled(); !provider429Chip.MatchString(s) {
			t.Errorf("no retry indication during the 429 backoff:\n%s", s)
		}
		a.key(uv.KeyEsc, 0)
		a.waitFor("■ cancelled")
	})

	t.Run("retry_after_honoured", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_PROVIDER429RETRYTHENCANCEL") == "" {
			t.Skip("known bug: Retry-After is ignored — a 429 saying \"rate limited\" waits rateLimitDelays[0]=5s instead of the 2s the header asked (plugins/llm/retry.go withRetries); set BOUGH_KNOWN_PROVIDER429RETRYTHENCANCEL=1 to run")
		}
		t.Parallel()
		srv := provider429NewServer(t)
		a := provider429Start(t, srv)
		first := provider429Backoff(t, a, srv)
		deadline := first.Add(4 * time.Second)
		for len(srv.times()) < 2 && time.Now().Before(deadline) {
			time.Sleep(20 * time.Millisecond)
		}
		got := srv.times()
		if len(got) < 2 {
			t.Errorf("no retry within 4s of a 429 with Retry-After: 2")
		} else if gap := got[1].Sub(got[0]); gap < 1500*time.Millisecond {
			t.Errorf("retried after %v, before Retry-After: 2 allowed", gap)
		}
		a.key(uv.KeyEsc, 0)
		a.waitFor("■ cancelled")
	})
}
