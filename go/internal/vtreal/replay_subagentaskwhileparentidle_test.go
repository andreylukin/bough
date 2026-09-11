package vtreal

// A spawned subagent calls tools.ask while the user sits in the parent
// view. The ask must surface there, an answer must reach the child
// (its sub:* history carries the answer it printed), and esc must
// decline only the child's ask: the child and then the parent carry on
// to their stop replies, nothing reads cancelled.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// subagentAskWhileParentIdleConfig is jobsConfig plus the workers row
// (tools.spawn) and the ask row with a short timeout.
func subagentAskWhileParentIdleConfig(tape string) string {
	cfg := jobsConfig(tape)
	out := strings.Replace(cfg, "- id: commands\n", "- id: workers\n  plugin: workers\n- id: ask\n  plugin: ask\n  config: {timeout_minutes: 1}\n- id: commands\n", 1)
	if out == cfg {
		panic("subagentAskWhileParentIdleConfig: jobsConfig changed shape")
	}
	return out
}

// subagentAskWhileParentIdleTape: the parent spawns one child, the
// child asks, then reports; the parent stops.
func subagentAskWhileParentIdleTape(t *testing.T) string {
	t.Helper()
	entries := []struct{ kind, text string }{
		{"input", "delegate the pick"},
		{"assistant", "```js\nconsole.log(tools.spawn(\"pick a shade\"))\n```"},
		{"assistant", "```js\nconsole.log(\"SUB-PICKED \" + tools.ask(\"Subagent shade?\", \"alpha\", \"beta\"))\n```"},
		{"assistant", "```stop\nCHILD-REPORT\n```"},
		{"assistant", "```stop\nPARENT-DONE\n```"},
	}
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{
			"seq": i + 1, "at": time.Date(2026, 9, 11, 11, 0, i, 0, time.UTC).Format(time.RFC3339),
			"kind": e.kind, "data": map[string]any{"text": e.text},
		})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// subagentAskWhileParentIdleStart boots, submits, and waits for the
// child's ask to show in the parent view.
func subagentAskWhileParentIdleStart(t *testing.T) *app {
	t.Helper()
	a := startCfg(t, 110, 40, subagentAskWhileParentIdleConfig(subagentAskWhileParentIdleTape(t)))
	jobsSay(a, "delegate the pick")
	a.waitFor("Subagent shade?")
	return a
}

// subagentAskWhileParentIdleChildSaw polls history for a sub:* entry
// whose data contains want (the child's block output).
func subagentAskWhileParentIdleChildSaw(a *app, want string) bool {
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
		for _, e := range jobsHistory(a) {
			if strings.HasPrefix(e.Kind, "sub:") && strings.Contains(fmt.Sprint(e.Data), want) {
				return true
			}
		}
	}
	return false
}

// subagentAskWhileParentIdleFinish waits for the parent's stop reply
// and checks nothing reads cancelled.
func subagentAskWhileParentIdleFinish(t *testing.T, a *app) {
	t.Helper()
	var s string
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
		if s = a.text(); strings.Contains(s, "PARENT-DONE") {
			break
		}
	}
	if !strings.Contains(s, "PARENT-DONE") {
		t.Fatalf("parent never reached its stop reply:\n%s", s)
	}
	for _, e := range jobsHistory(a) {
		if e.Kind == "cancelled" || (e.Kind == "sub:done" && e.Data["status"] == "cancelled") {
			t.Errorf("turn or child cancelled: %+v", e)
		}
	}
}

func TestSubagentAskWhileParentIdle(t *testing.T) {
	t.Parallel()

	t.Run("surfaces_in_parent", func(t *testing.T) {
		t.Parallel()
		a := subagentAskWhileParentIdleStart(t)
		s := a.settled()
		for _, want := range []string{"Subagent shade?", "alpha", "beta", askPendingPlaceholder} {
			if !strings.Contains(s, want) {
				t.Errorf("parent view missing %q:\n%s", want, s)
			}
		}
	})

	t.Run("answer_routes_to_child", func(t *testing.T) {
		t.Parallel()
		a := subagentAskWhileParentIdleStart(t)
		a.settled()
		a.typeText("2")
		a.key(uv.KeyEnter, 0)
		if !subagentAskWhileParentIdleChildSaw(a, "SUB-PICKED beta") {
			t.Fatalf("child history never carried the answer:\n%s", a.text())
		}
		subagentAskWhileParentIdleFinish(t, a)
	})

	t.Run("esc_declines_only_child_ask", func(t *testing.T) {
		t.Parallel()
		a := subagentAskWhileParentIdleStart(t)
		a.settled()
		a.key(uv.KeyEscape, 0)
		if !subagentAskWhileParentIdleChildSaw(a, "SUB-PICKED (declined)") {
			t.Fatalf("child never got (declined):\n%s", a.text())
		}
		subagentAskWhileParentIdleFinish(t, a)
	})
}
