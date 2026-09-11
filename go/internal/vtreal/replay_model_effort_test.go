package vtreal

// /model picker and shift+tab thinking levels on a replay session.
// Neither may call the model: every tape reply must still be there for
// the first real turn.

import (
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// modelEffortAssistants counts assistant entries in this run's history:
// anything above zero before a turn is sent means a tape reply was eaten.
func (a *app) modelEffortAssistants() int {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	n := 0
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			if e.Kind == "assistant" {
				n++
			}
		}
	}
	return n
}

// modelEffortStatus is the status bar: the last row ending in "? keys".
func (a *app) modelEffortStatus() string {
	ls := a.lines()
	for i := len(ls) - 1; i >= 0; i-- {
		if strings.HasSuffix(ls[i], "? keys") {
			return ls[i]
		}
	}
	return ""
}

func modelEffortStart(t *testing.T) *app {
	tape, _ := filepath.Abs("testdata/replay/basic.jsonl")
	a := startCfg(t, 120, 30, replayConfig(tape))
	a.check("boot")
	return a
}

func (a *app) modelEffortOpenPicker() {
	a.typeText("/model")
	a.key(uv.KeyEnter, 0)
	a.waitFor("pick a model")
}

var modelEffortCursor = regexp.MustCompile(`(?m)^▸ (.+?)( \(current\))?$`)

func TestModelEffortPickerRows(t *testing.T) {
	t.Parallel()
	a := modelEffortStart(t)
	a.modelEffortOpenPicker()
	s := a.settled()
	// The replay row has no model, so the current row is the bare
	// plugin name, marked and under the cursor.
	if !strings.Contains(s, "▸ replay (current)") {
		t.Fatalf("cursor not on the current replay row:\n%s", s)
	}
	if !regexp.MustCompile(`(?m)^  llm-\S+`).MatchString(s) {
		t.Errorf("no provider rows under the current one:\n%s", s)
	}
	a.typeText("echo")
	a.waitFor("▸ llm-echo")
	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "picker to close")
	a.check("after esc")
	if n := a.modelEffortAssistants(); n != 0 {
		t.Fatalf("opening /model consumed %d tape replies:\n%s", n, a.text())
	}
	// The first turn still gets the tape's first reply.
	a.typeText("list the files here")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("Two Go files")
	a.check("after turn")
}

func TestModelEffortNoLevelOnReplay(t *testing.T) {
	t.Parallel()
	a := modelEffortStart(t)
	a.key(uv.KeyTab, uv.ModShift)
	a.waitFor("this provider has no thinking level to set")
	if st := a.modelEffortStatus(); strings.Contains(st, "think ") {
		t.Errorf("think chip on a provider with no level: %q\n%s", st, a.text())
	}
	a.check("after shift+tab")
	if n := a.modelEffortAssistants(); n != 0 {
		t.Fatalf("shift+tab consumed %d tape replies:\n%s", n, a.text())
	}
}

// TestModelEffortSwapAndCycle picks a real provider (it mounts without
// a network; nothing is sent), then walks every thinking level. Replay
// and echo implement neither llm.Modeler nor llm.Efforter, so this is
// the only way to reach the chips offline.
func TestModelEffortSwapAndCycle(t *testing.T) {
	t.Parallel()
	a := modelEffortStart(t)
	a.modelEffortOpenPicker()
	a.typeText("llm-openai")
	a.waitUntil(func(s string) bool {
		m := modelEffortCursor.FindStringSubmatch(s)
		return m != nil && strings.HasPrefix(m[1], "llm-openai ")
	}, "cursor on an llm-openai row")
	choice := modelEffortCursor.FindStringSubmatch(a.text())[1]
	prov, mdl, _ := strings.Cut(choice, " ")
	a.key(uv.KeyEnter, 0)
	a.waitFor("model: " + prov + " · " + mdl)
	a.waitUntil(func(string) bool { return strings.Contains(a.modelEffortStatus(), mdl) },
		"status chip to name "+mdl)
	a.check("after swap")

	for _, lvl := range []string{"off", "low", "medium", "high", "xhigh", ""} {
		label := lvl
		if label == "" {
			label = "default"
		}
		a.key(uv.KeyTab, uv.ModShift)
		a.waitFor("thinking: " + label + " (from the next message)")
		// The flash covers the chips until the next key; type and erase
		// one character so the draft stays empty.
		a.typeText("x")
		a.waitFor("> x")
		a.key(uv.KeyBackspace, 0)
		a.waitUntil(func(string) bool {
			st := a.modelEffortStatus()
			return !strings.Contains(st, "thinking:") && strings.Contains(st, mdl)
		}, "flash to clear after "+label)
		st := a.modelEffortStatus()
		if want := "think " + lvl; lvl != "" && !strings.Contains(st, want) {
			t.Errorf("level %s: status bar lacks %q: %q\n%s", label, want, st, a.text())
		}
		if lvl == "" && strings.Contains(st, "think ") {
			t.Errorf("back at default but a think chip remains: %q\n%s", st, a.text())
		}
	}
	a.check("after cycle")
	if n := a.modelEffortAssistants(); n != 0 || a.doneCount() != 0 {
		t.Fatalf("picker/effort ran a turn (%d assistant entries):\n%s", n, a.text())
	}
}
