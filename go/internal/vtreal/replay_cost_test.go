package vtreal

// The cost row end to end: a fake OpenAI Responses server on localhost
// reports fixed usage per request, the cost row prices it from its own
// `prices`, and the status bar, /cost and the done entries must agree.
// A recorded session whose done entries carry usage checks the resumed
// tally, and /sessions into it checks that the process-wide llm tally
// does not leak into another session (bough-cost-session-leak).
//
// No replay llm here: the replay plugin reports no usage (it is not an
// llm.UsageReporter), so the cost row would mount as a no-op on it.

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// Per request the fake server reports costIn/costOut tokens; at the
// cost row's $10/$50 per Mtok that is $0.64 + $0.10. gpt-4o-mini's
// 128k window makes last_in 64k read "50% ctx".
const (
	costIn   = 64000
	costOut  = 2000
	costCall = 0.74
)

// The fixture's two done entries: 132k in, 5k out, $1.57, last 32k.
const costBase = 1.57

// costServer answers every /v1/responses call with a stop block and the
// fixed usage, counting calls so a stray model call shows up.
func costServer(t *testing.T) (*httptest.Server, *atomic.Int32) {
	var calls atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/responses" {
			http.NotFound(w, r)
			return
		}
		calls.Add(1)
		text := "Done.\n\n```stop\nDone.\n```"
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

// costConfig points the llm row at srv, prices gpt-4o-mini on the cost
// row, and turns off every row that calls the model or the network on
// its own.
func costConfig(srv, historyFile string) string {
	hist := ""
	if historyFile != "" {
		hist = fmt.Sprintf("\n  config: {file: %q}", historyFile)
	}
	return fmt.Sprintf(`
- id: llm
  plugin: llm-openai
  config: {model: gpt-4o-mini, base_url: %q}
- id: history
  plugin: history%s
- id: cost
  plugin: cost
  config: {prices: {gpt-4o-mini: {input: 10, output: 50}}}
- id: session-title
  plugin: session-title
  disabled: true
- id: auto-memory
  plugin: auto-memory
  disabled: true
- id: memory-tier
  plugin: memory-tier
  disabled: true
- id: activity
  plugin: activity
  disabled: true
- id: attention
  plugin: attention
  disabled: true
- id: collect
  plugin: collect
  disabled: true
`, srv, hist)
}

// costHome is a fresh $HOME holding a models.json fresh enough that the
// catalogue never refreshes from models.dev, and the resume fixture as
// session "costold".
func costHome(t *testing.T) (home, session string) {
	home = t.TempDir()
	dir := filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	// Non-empty (an empty one counts as missing) with no openai entry,
	// so the price and the context window come from the cost row alone.
	stub := `{"v":2,"c":{"costtest":{"none":{}}}}`
	if err := os.WriteFile(filepath.Join(home, ".bough", "models.json"), []byte(stub), 0o644); err != nil {
		t.Fatal(err)
	}
	tape, err := os.ReadFile("testdata/replay/cost_resume.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	session = filepath.Join(dir, "costold.jsonl")
	if err := os.WriteFile(session, tape, 0o644); err != nil {
		t.Fatal(err)
	}
	return home, session
}

// costStart is startCfg over a prepared $HOME, with a key for the fake
// provider.
func costStart(t *testing.T, home, yml string) *app {
	t.Helper()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	const cols, rows = 120, 30
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "OPENAI_API_KEY=test-key",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
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

// costBar waits for the status bar (the row with "? keys") to show
// every chip.
func (a *app) costBar(where string, chips ...string) {
	a.t.Helper()
	a.waitUntil(func(s string) bool {
		bar := ""
		for _, l := range strings.Split(s, "\n") {
			if strings.Contains(l, "? keys") {
				bar = l
			}
		}
		for _, c := range chips {
			if !strings.Contains(bar, c) {
				return false
			}
		}
		return true
	}, fmt.Sprintf("%s: status bar shows %q", where, chips))
}

// costSend types a line, presses enter and waits for the n-th done.
func (a *app) costSend(line string, n int) {
	a.t.Helper()
	a.typeText(line)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(n, 30*time.Second) {
		a.t.Fatalf("turn %q never finished:\n%s", line, a.text())
	}
}

// costCommand runs a slash command and waits for its output.
func (a *app) costCommand(cmd, want string) {
	a.t.Helper()
	a.typeText(cmd)
	a.key(uv.KeyEnter, 0)
	a.waitFor(want)
}

// costUsages is the usage data of every done entry in path.
func costUsages(t *testing.T, path string) []map[string]any {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	var out []map[string]any
	for _, e := range entries {
		if e.Kind == "done" {
			u, _ := e.Data["usage"].(map[string]any)
			out = append(out, u)
		}
	}
	return out
}

func costMoney(c float64) string { return fmt.Sprintf("$%.3f", c) }

func TestCost(t *testing.T) {
	t.Parallel()

	t.Run("LiveTurnChipsAndDoneUsage", func(t *testing.T) {
		t.Parallel()
		srv, calls := costServer(t)
		home, _ := costHome(t)
		a := costStart(t, home, costConfig(srv.URL, ""))
		a.costSend("hello", 1)
		a.costBar("after one turn", costMoney(costCall), "50% ctx", "↑64.0k ↓2.0k")
		a.costCommand("/cost", "cost: 64.0k in · 2.0k out · $0.7400 — the cost row's prices")
		if n := calls.Load(); n != 1 {
			t.Errorf("model called %d times for one turn, want 1:\n%s", n, a.text())
		}
		// The done entry carries what the turn spent.
		paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
		var got []map[string]any
		for _, p := range paths {
			if filepath.Base(p) != "costold.jsonl" {
				got = append(got, costUsages(t, p)...)
			}
		}
		if len(got) != 1 || got[0] == nil {
			t.Fatalf("done entries' usage = %v, want one:\n%s", got, a.text())
		}
		u := got[0]
		if u["in"] != float64(costIn) || u["out"] != float64(costOut) || u["last_in"] != float64(costIn) {
			t.Errorf("done usage = %v, want in %d out %d last_in %d:\n%s", u, costIn, costOut, costIn, a.text())
		}
		if c, _ := u["cost"].(float64); fmt.Sprintf("%.4f", c) != "0.7400" {
			t.Errorf("done usage cost = %v, want 0.74:\n%s", u["cost"], a.text())
		}
		a.check("live")
	})

	t.Run("ResumedSumsItsFile", func(t *testing.T) {
		t.Parallel()
		srv, calls := costServer(t)
		home, session := costHome(t)
		a := costStart(t, home, costConfig(srv.URL, session))
		// Before any turn: the file's tally, and the context the next
		// turn starts from (last_in 32k of 128k).
		a.costBar("resumed, before a turn", costMoney(costBase), "25% ctx", "↑132.0k ↓5.0k")
		a.costCommand("/cost", "cost: 132.0k in · 5.0k out · $1.5700")
		a.costSend("one more", 3) // the file already holds two dones
		a.costBar("resumed, after a turn", costMoney(costBase+costCall), "50% ctx", "↑196.0k ↓7.0k")
		if n := calls.Load(); n != 1 {
			t.Errorf("model called %d times, want 1:\n%s", n, a.text())
		}
		if us := costUsages(t, session); len(us) != 3 || us[2] == nil || us[2]["in"] != float64(costIn) {
			t.Errorf("resumed file's done usages = %v, want the new turn's delta last (in %d), not the running total:\n%s", us, costIn, a.text())
		}
		a.check("resumed")
	})

	// bough-cost-session-leak: the llm's tally is per process, so a
	// session swapped in after a paid turn must not inherit that turn.
	t.Run("SessionSwapDoesNotLeak", func(t *testing.T) {
		t.Parallel()
		srv, _ := costServer(t)
		home, _ := costHome(t)
		a := costStart(t, home, costConfig(srv.URL, ""))
		a.costSend("spend here", 1)
		a.costBar("fresh session", costMoney(costCall))
		a.typeText("/sessions costold")
		a.key(uv.KeyEnter, 0)
		a.costBar("after /sessions costold", costMoney(costBase), "25% ctx", "↑132.0k ↓5.0k")
		if s := a.settled(); strings.Contains(s, costMoney(costBase+costCall)) {
			t.Errorf("the previous session's spend leaked into the resumed one:\n%s", s)
		}
		a.costCommand("/cost", "cost: 132.0k in · 5.0k out · $1.5700")
		a.check("swapped")
	})
}
