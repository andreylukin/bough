package vtreal

// Transient provider failures, end to end: a fake OpenAI Responses
// server answers a turn's first calls with 429/500 and then succeeds.
// llm.withRetries retries them inside one model call, so the turn must
// show one reply, count its cost once and record one assistant entry;
// the user should also be told the provider is being retried. Three
// failures in a row spend the retry budget: the turn ends in an error
// block and the composer takes the next turn.
//
// The replay llm cannot stand in here: retries live in the provider's
// HTTP path (plugins/llm/retry.go), which the replay plugin skips.

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"regexp"
	"strings"
	"sync/atomic"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// providerRetryVisibleWord appears once in the successful reply's prose
// and nowhere else, so counting it on screen catches a doubled reply.
const providerRetryVisibleWord = "ZEPHYR42"

// providerRetryVisibleServer fails call i (0-based) with fails[i] when
// i < len(fails), and otherwise answers like costServer with the fixed
// usage, so the cost row prices one success at costCall.
func providerRetryVisibleServer(t *testing.T, fails ...int) (*httptest.Server, *atomic.Int32) {
	var calls atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/responses" {
			http.NotFound(w, r)
			return
		}
		i := int(calls.Add(1)) - 1
		if i < len(fails) {
			// No "rate limited" wording: that switches withRetries to its
			// minutes-long rate-limit budget.
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(fails[i])
			fmt.Fprint(w, `{"error":{"message":"transient hiccup"}}`)
			return
		}
		text := providerRetryVisibleWord + " is the answer.\n\n```stop\nDone.\n```"
		completed, _ := json.Marshal(map[string]any{
			"type": "response.completed",
			"response": map[string]any{
				"status": "completed",
				"output": []any{map[string]any{"type": "message",
					"content": []any{map[string]any{"type": "output_text", "text": text}}}},
				"usage": map[string]any{"input_tokens": costIn, "output_tokens": costOut},
			},
		})
		delta, _ := json.Marshal(map[string]any{"type": "response.output_text.delta", "delta": text})
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprintf(w, "data: %s\n\ndata: %s\n\ndata: [DONE]\n\n", delta, completed)
	}))
	t.Cleanup(srv.Close)
	return srv, &calls
}

// providerRetryVisibleEntries is every entry of the session this run
// wrote (every file but the costold fixture), filtered to kind.
func providerRetryVisibleEntries(t *testing.T, home, kind string) []history.Entry {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	var out []history.Entry
	for _, p := range paths {
		if filepath.Base(p) == "costold.jsonl" {
			continue
		}
		es, err := history.Read(p)
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range es {
			if e.Kind == kind {
				out = append(out, e)
			}
		}
	}
	return out
}

var providerRetryVisibleNotice = regexp.MustCompile(`(?i)retry|retrying|attempt [0-9]`)

func TestProviderRetryVisible(t *testing.T) {
	t.Parallel()

	// A 429 then a 500, then success: one reply, one cost, one entry.
	t.Run("TransientThenSuccess", func(t *testing.T) {
		t.Parallel()
		srv, calls := providerRetryVisibleServer(t, http.StatusTooManyRequests, http.StatusInternalServerError)
		home, _ := costHome(t)
		a := costStart(t, home, costConfig(srv.URL, ""))
		a.costSend("hello")
		a.waitFor(providerRetryVisibleWord)
		a.costBar("after the retried turn", costMoney(costCall), "↑64.0k ↓2.0k")
		s := a.settled()
		if n := strings.Count(s, providerRetryVisibleWord); n != 1 {
			t.Errorf("reply shown %d times, want 1:\n%s", n, s)
		}
		if strings.Contains(s, "✗") || strings.Contains(s, "HTTP 429") || strings.Contains(s, "HTTP 500") {
			t.Errorf("a retried failure surfaced as an error:\n%s", s)
		}
		if n := calls.Load(); n != 3 {
			t.Errorf("provider called %d times, want 3 (429, 500, success)", n)
		}
		a.costCommand("/cost", "cost: 64.0k in · 2.0k out · $0.7400")
		if as := providerRetryVisibleEntries(t, home, "assistant"); len(as) != 1 {
			t.Errorf("history has %d assistant entries, want 1: %v", len(as), as)
		}
		if ds := providerRetryVisibleEntries(t, home, "done"); len(ds) != 1 {
			t.Errorf("history has %d done entries, want 1", len(ds))
		} else if u, _ := ds[0].Data["usage"].(map[string]any); u == nil || u["in"] != float64(costIn) {
			t.Errorf("done usage = %v, want in %d (counted once)", ds[0].Data["usage"], costIn)
		}
		if es := providerRetryVisibleEntries(t, home, "error"); len(es) != 0 {
			t.Errorf("history has error entries for a turn that succeeded: %v", es)
		}
		a.check("retried turn")
	})

	// The user waits seconds between attempts; something must say why.
	t.Run("RetryNoticeShown", func(t *testing.T) {
		t.Parallel()
		srv, _ := providerRetryVisibleServer(t, http.StatusInternalServerError, http.StatusInternalServerError)
		home, _ := costHome(t)
		a := costStart(t, home, costConfig(srv.URL, ""))
		a.typeText("hello")
		a.key(uv.KeyEnter, 0)
		// The second attempt is 1 s after the first and the third 3 s
		// after that: a notice has seconds to show.
		a.waitUntil(func(s string) bool {
			return providerRetryVisibleNotice.MatchString(s)
		}, "a retry notice on screen while the provider is retried")
	})

	// Three failures spend the budget: an error block, then the
	// composer still takes a turn that succeeds.
	t.Run("ThreeFailuresThenComposer", func(t *testing.T) {
		t.Parallel()
		srv, calls := providerRetryVisibleServer(t, 500, 500, 500)
		home, _ := costHome(t)
		a := costStart(t, home, costConfig(srv.URL, ""))
		a.costSend("hello")
		a.waitFor("HTTP 500")
		errorsWant(a, "failed turn", "✗", "transient hiccup")
		if n := calls.Load(); n != 3 {
			t.Errorf("provider called %d times, want 3", n)
		}
		a.check("failed turn")
		a.costSend("try again")
		a.waitFor(providerRetryVisibleWord)
		if n := calls.Load(); n != 4 {
			t.Errorf("provider called %d times after the follow-up, want 4", n)
		}
		if as := providerRetryVisibleEntries(t, home, "assistant"); len(as) != 1 {
			t.Errorf("history has %d assistant entries, want 1 (the follow-up's)", len(as))
		}
		a.check("follow-up turn")
	})
}
