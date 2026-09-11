package vtreal

// The pinned todo panel under a burst of todo updates while ctrl+t
// toggles it. Replayed blocks answer from the recording without
// running, so the updates come from /todo mutations on the resumed
// todo tape; the sync point is the count of todo entries the history
// row wrote, never the screen the test is judging.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// todoPanelConcurrentUpdatesStart is todoStart that also returns the
// resumed history file the session appends its todo entries to.
func todoPanelConcurrentUpdatesStart(t *testing.T, cols, rows int) (*app, string) {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/todo.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	src, err := os.ReadFile(tape)
	if err != nil {
		t.Fatal(err)
	}
	resume := filepath.Join(t.TempDir(), "todo.jsonl")
	if err := os.WriteFile(resume, src, 0o644); err != nil {
		t.Fatal(err)
	}
	yml := strings.Replace(replayConfig(tape),
		"- id: history\n  plugin: history",
		"- id: history\n  plugin: history\n  config: {file: "+resume+"}", 1)
	if !strings.Contains(yml, "config: {file: "+resume) {
		t.Fatalf("could not point the history row at %s", resume)
	}
	return startCfg(t, cols, rows, yml), resume
}

// todoPanelConcurrentUpdatesCount is how many todo/* entries the
// session file holds.
func todoPanelConcurrentUpdatesCount(path string) int {
	es, err := history.Read(path)
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range es {
		if strings.HasPrefix(e.Kind, "todo/") {
			n++
		}
	}
	return n
}

func todoPanelConcurrentUpdatesWait(a *app, path string, n int) {
	a.t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for todoPanelConcurrentUpdatesCount(path) < n {
		if time.Now().After(deadline) {
			a.t.Fatalf("todo entries never reached %d (have %d):\n%s", n, todoPanelConcurrentUpdatesCount(path), a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// todoPanelConcurrentUpdatesOps is 20 updates: 16 adds (items 4..19)
// and 4 dones (1, 3, 5, 7), ending on a list of 19 items.
func todoPanelConcurrentUpdatesOps() []string {
	var ops []string
	for i := 4; i <= 19; i++ {
		ops = append(ops, fmt.Sprintf("/todo add item %d", i))
		if i%4 == 0 {
			ops = append(ops, fmt.Sprintf("/todo done %d", i/4*2-1))
		}
	}
	return ops
}

// todoPanelConcurrentUpdatesPanel is the panel rows on screen: the
// header and every checkbox or "+N more" row under it.
func todoPanelConcurrentUpdatesPanel(s string) []string {
	var out []string
	for _, l := range strings.Split(s, "\n") {
		l = strings.TrimSpace(l)
		if strings.Contains(l, todoHeader) || strings.HasPrefix(l, "[ ] ") ||
			strings.HasPrefix(l, "[x] ") || (strings.HasPrefix(l, "+") && strings.HasSuffix(l, " more")) {
			out = append(out, l)
		}
	}
	return out
}

func todoPanelConcurrentUpdatesWant() []string {
	return []string{
		todoHeader,
		"[x] 1 cut the tag",
		"[x] 2 review the diff",
		"[x] 3 write the notes",
		"[ ] 4 item 4",
		"[x] 5 item 5",
		"[ ] 6 item 6",
		"[x] 7 item 7",
		"[ ] 8 item 8",
		"+11 more",
	}
}

func todoPanelConcurrentUpdatesAssert(t *testing.T, a *app, where string) {
	t.Helper()
	s := a.settled()
	got := strings.Join(todoPanelConcurrentUpdatesPanel(s), "\n")
	want := strings.Join(todoPanelConcurrentUpdatesWant(), "\n")
	if got != want {
		t.Fatalf("%s: panel is not the last todo state\ngot:\n%s\nwant:\n%s\nscreen:\n%s", where, got, want, s)
	}
	a.check(where)
}

func todoPanelConcurrentUpdatesAssertHidden(t *testing.T, a *app, where string) {
	t.Helper()
	s := a.settled()
	if got := todoPanelConcurrentUpdatesPanel(s); len(got) > 0 {
		t.Fatalf("%s: hidden panel left rows behind:\n%s\nscreen:\n%s", where, strings.Join(got, "\n"), s)
	}
	a.check(where)
}

func TestTodoPanelConcurrentUpdates(t *testing.T) {
	t.Parallel()
	ops := todoPanelConcurrentUpdatesOps()
	if len(ops) != 20 {
		t.Fatalf("want 20 updates, have %d", len(ops))
	}
	const base = 4 // the tape's three adds and one done

	// Lockstep: every update lands before the next key; ctrl+t after
	// each one, so the panel is hidden after odd updates, shown after
	// even ones.
	t.Run("lockstep", func(t *testing.T) {
		t.Parallel()
		a, hist := todoPanelConcurrentUpdatesStart(t, 100, 30)
		a.waitFor(todoHeader)
		for i, op := range ops {
			a.typeText(op)
			a.key(uv.KeyEnter, 0)
			todoPanelConcurrentUpdatesWait(a, hist, base+i+1)
			a.key('t', uv.ModCtrl)
			if i%2 == 0 {
				a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the panel to hide")
			} else {
				a.waitFor(todoHeader)
			}
		}
		todoPanelConcurrentUpdatesAssert(t, a, "after 20 updates, shown")
		a.key('t', uv.ModCtrl)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the panel to hide")
		todoPanelConcurrentUpdatesAssertHidden(t, a, "hidden after the updates")
		a.key('t', uv.ModCtrl)
		a.waitFor(todoHeader)
		todoPanelConcurrentUpdatesAssert(t, a, "shown again")
	})

	// Burst: all 20 updates and 20 toggles typed back to back, then
	// one sync on the entry count. An even number of toggles leaves
	// the panel shown, and it must show the last state.
	t.Run("burst", func(t *testing.T) {
		t.Parallel()
		a, hist := todoPanelConcurrentUpdatesStart(t, 100, 30)
		a.waitFor(todoHeader)
		for _, op := range ops {
			a.typeText(op)
			a.key(uv.KeyEnter, 0)
			a.key('t', uv.ModCtrl)
		}
		todoPanelConcurrentUpdatesWait(a, hist, base+len(ops))
		a.waitFor("+11 more")
		todoPanelConcurrentUpdatesAssert(t, a, "after the burst")
		a.key('t', uv.ModCtrl)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the panel to hide")
		todoPanelConcurrentUpdatesAssertHidden(t, a, "hidden after the burst")
	})
}
