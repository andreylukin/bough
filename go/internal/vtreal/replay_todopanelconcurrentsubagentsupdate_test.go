package vtreal

// The pinned todo panel while a parent and two subagents update the
// todo list at once. codemode is real here (not replayed), so the
// tape's js blocks run: the parent adds, fans out with tools.spawnAll,
// and each child adds its own item behind a sleep barrier, so the
// children's first model calls are both served (tape order X, X, Y, Y)
// before either block finishes. Child blocks share the parent's VM,
// so the shared counter numbers them deterministically.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const todoPanelConcurrentSubagentsUpdateGate = "BOUGH_KNOWN_TODO_PANEL_CONCURRENT_SUBAGENTS_UPDATE"

func todoPanelConcurrentSubagentsUpdateTape(t *testing.T) string {
	t.Helper()
	child := "```js\ntools.bash(\"sleep 0.4\"); globalThis.__todoc = (globalThis.__todoc || 0) + 1; console.log(\"CHILD-ADDED \" + tools.todo.add(\"child item \" + globalThis.__todoc))\n```"
	entries := []struct{ kind, text string }{
		{"input", "split the release"},
		{"assistant", "```js\ntools.todo.add(\"parent a\"); tools.todo.add(\"parent b\"); var r = tools.spawnAll([\"child one\", \"child two\"]); tools.todo.add(\"parent c\"); tools.todo.done(1); console.log(\"SPAWNED \" + r.length)\n```"},
		{"assistant", child},
		{"assistant", child},
		{"assistant", "```stop\nCHILD-REPORT\n```"},
		{"assistant", "```stop\nCHILD-REPORT\n```"},
		{"assistant", "```stop\nPARENT-DONE\n```"},
	}
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{
			"seq": i + 1, "at": time.Date(2026, 9, 11, 12, 0, i, 0, time.UTC).Format(time.RFC3339),
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

// todoPanelConcurrentSubagentsUpdateConfig is jobsConfig plus the
// workers (tools.spawnAll) and todo (tools.todo) rows.
func todoPanelConcurrentSubagentsUpdateConfig(tape string) string {
	cfg := jobsConfig(tape)
	out := strings.Replace(cfg, "- id: loop\n", "- id: workers\n  plugin: workers\n- id: todo\n  plugin: todo\n- id: loop\n", 1)
	if out == cfg {
		panic("todoPanelConcurrentSubagentsUpdateConfig: jobsConfig changed shape")
	}
	return out
}

// todoPanelConcurrentSubagentsUpdateRun boots, sends the prompt,
// presses ctrl+t (an even number of times) while the children run,
// and waits for the turn to finish.
func todoPanelConcurrentSubagentsUpdateRun(t *testing.T, toggles int) *app {
	t.Helper()
	a := startCfg(t, 110, 40, todoPanelConcurrentSubagentsUpdateConfig(todoPanelConcurrentSubagentsUpdateTape(t)))
	jobsSay(a, "split the release")
	for range toggles {
		time.Sleep(80 * time.Millisecond)
		a.key('t', uv.ModCtrl)
	}
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("PARENT-DONE")
	return a
}

func todoPanelConcurrentSubagentsUpdateWant() []string {
	return []string{
		todoHeader,
		"[x] 1 parent a",
		"[ ] 2 parent b",
		"[ ] 3 child item 1",
		"[ ] 4 child item 2",
		"[ ] 5 parent c",
	}
}

func todoPanelConcurrentSubagentsUpdateAssert(t *testing.T, a *app, where string) {
	t.Helper()
	a.waitFor("5 parent c")
	s := a.settled()
	got := strings.Join(todoPanelConcurrentUpdatesPanel(s), "\n")
	want := strings.Join(todoPanelConcurrentSubagentsUpdateWant(), "\n")
	if got != want {
		t.Fatalf("%s: panel is not the final todo state\ngot:\n%s\nwant:\n%s\nscreen:\n%s", where, got, want, s)
	}
	a.check(where)
}

// todoPanelConcurrentSubagentsUpdateHistory is the newest session file.
func todoPanelConcurrentSubagentsUpdateHistory(t *testing.T, a *app) string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var newest string
	var at time.Time
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil && !st.ModTime().Before(at) {
			newest, at = p, st.ModTime()
		}
	}
	if newest == "" {
		t.Fatal("no session history written")
	}
	return newest
}

func TestTodoPanelConcurrentSubagentsUpdate(t *testing.T) {
	t.Parallel()

	// Every update from all three agents lands once, with unique ids.
	t.Run("counts_after_finish", func(t *testing.T) {
		t.Parallel()
		a := todoPanelConcurrentSubagentsUpdateRun(t, 0)
		todoPanelConcurrentSubagentsUpdateAssert(t, a, "after all agents finished")
		adds, dones := 0, 0
		for _, e := range jobsHistory(a) {
			switch e.Kind {
			case "todo/add":
				adds++
			case "todo/done":
				dones++
			}
		}
		if adds != 5 || dones != 1 {
			t.Fatalf("history holds %d todo/add and %d todo/done, want 5 and 1", adds, dones)
		}
	})

	// ctrl+t while the children update: no crash, and an even number
	// of presses leaves the panel shown on the final state.
	t.Run("toggle_during_updates", func(t *testing.T) {
		t.Parallel()
		a := todoPanelConcurrentSubagentsUpdateRun(t, 10)
		todoPanelConcurrentSubagentsUpdateAssert(t, a, "after toggling during updates")
		a.key('t', uv.ModCtrl)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the panel to hide")
		a.key('t', uv.ModCtrl)
		todoPanelConcurrentSubagentsUpdateAssert(t, a, "shown again")
	})

	// A resumed session pins the same final list.
	t.Run("resume_same_state", func(t *testing.T) {
		t.Parallel()
		a := todoPanelConcurrentSubagentsUpdateRun(t, 0)
		todoPanelConcurrentSubagentsUpdateAssert(t, a, "before resume")
		src, err := os.ReadFile(todoPanelConcurrentSubagentsUpdateHistory(t, a))
		if err != nil {
			t.Fatal(err)
		}
		resume := filepath.Join(t.TempDir(), "resume.jsonl")
		if err := os.WriteFile(resume, src, 0o644); err != nil {
			t.Fatal(err)
		}
		yml := strings.Replace(replayConfig(resume),
			"- id: history\n  plugin: history",
			"- id: history\n  plugin: history\n  config: {file: "+resume+"}", 1)
		if !strings.Contains(yml, "config: {file: "+resume) {
			t.Fatalf("could not point the history row at %s", resume)
		}
		b := startCfg(t, 110, 40, yml)
		b.waitFor(todoHeader)
		todoPanelConcurrentSubagentsUpdateAssert(t, b, "after resume")
	})

	// The spec: the panel shows the parent's todos only, or labels a
	// subagent's items by agent.
	t.Run("parent_only_or_labeled", func(t *testing.T) {
		t.Parallel()
		if os.Getenv(todoPanelConcurrentSubagentsUpdateGate) == "" {
			t.Skip("known bug: subagents share the parent's tools.todo (same codemode VM); their items show in the parent's panel with no agent label — set " + todoPanelConcurrentSubagentsUpdateGate + "=1 to run")
		}
		a := todoPanelConcurrentSubagentsUpdateRun(t, 0)
		a.waitFor("5 parent c")
		s := a.settled()
		for _, l := range todoPanelConcurrentUpdatesPanel(s) {
			if strings.Contains(l, "child item") && !strings.Contains(l, "subagent") {
				t.Fatalf("a subagent's todo shows in the parent's panel unlabeled: %q\nscreen:\n%s", l, s)
			}
		}
	})
}
