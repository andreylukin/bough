package vtreal

// initjs-hot-reload-during-ask: the tape's block calls tools.ask. With
// the ask pending on screen, the test rewrites ~/.bough/init.js three
// times: to a file registering a command and rebinding clear_input to
// ctrl+g, to a file that throws, and back to the good file. What must
// hold: the ask stays pending and unanswered through every edit, the
// throwing file's error is shown without dropping the ask, "2" still
// answers it, and afterwards the new command runs and ctrl+g clears.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const initJsHotReloadDuringAskGood = `
bough.setup({ui: {keymap: {clear_input: "ctrl+g"}}})
bough.command("hotask-cmd", "[x]", "hotask summary", function (args) {
	return "HOTASK_CMD " + args
})
`

const initJsHotReloadDuringAskBad = `throw new Error("HOTASK_BOOM")
`

// initJsHotReloadDuringAskTape writes a tape whose block asks at once.
func initJsHotReloadDuringAskTape(t *testing.T) string {
	t.Helper()
	code := "const c = tools.ask(\"Pick a color\", \"chartreuse\", \"vermilion\");\nconsole.log(\"you picked \" + c);\n"
	var sb strings.Builder
	for i, e := range []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "pick a color"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"assistant", map[string]any{"text": "```stop\nColor locked in.\n```"}},
		{"done", map[string]any{}},
	} {
		b, _ := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": e.kind, "data": e.data})
		sb.Write(append(b, '\n'))
	}
	p := filepath.Join(t.TempDir(), "initjs_hot_reload_during_ask.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// initJsHotReloadDuringAskPhase runs one phase with a.t pointed at the
// subtest, so a phase failure fails that subtest, not the parent.
func initJsHotReloadDuringAskPhase(t *testing.T, a *app, name string, f func(t *testing.T)) {
	t.Run(name, func(t *testing.T) {
		parent := a.t
		a.t = t
		defer func() { a.t = parent }()
		f(t)
	})
}

// initJsHotReloadDuringAskPending asserts the ask is on screen and has
// no recorded answer.
func initJsHotReloadDuringAskPending(t *testing.T, a *app, where string) {
	t.Helper()
	time.Sleep(1500 * time.Millisecond) // past any watcher debounce
	s := a.settled()
	for _, want := range []string{"? Pick a color", "chartreuse", "vermilion", askPendingPlaceholder} {
		if !strings.Contains(s, want) {
			t.Errorf("%s: pending ask missing %q:\n%s", where, want, s)
		}
	}
	if got := askDuringSessionPickerEntries(a, "ask/answer"); len(got) != 0 {
		t.Errorf("%s: ask answered by an init.js edit: %q\n%s", where, got, s)
	}
}

func TestInitJsHotReloadDuringAsk(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, askConfig(initJsHotReloadDuringAskTape(t)))
	path := filepath.Join(a.home, ".bough", "init.js")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	write := func(body string) {
		if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}

	a.typeText("pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitFor(askPendingPlaceholder)

	write(initJsHotReloadDuringAskGood)
	initJsHotReloadDuringAskPhase(t, a, "ask pending after good edit", func(t *testing.T) {
		initJsHotReloadDuringAskPending(t, a, "after good edit")
	})

	write(initJsHotReloadDuringAskBad)
	initJsHotReloadDuringAskPhase(t, a, "throwing edit shows error", func(t *testing.T) {
		a.waitUntil(func(s string) bool {
			return strings.Contains(s, "init.js") && strings.Contains(s, "HOTASK_BOOM")
		}, "the init.js error while the ask is pending")
	})
	initJsHotReloadDuringAskPhase(t, a, "ask pending after throwing edit", func(t *testing.T) {
		initJsHotReloadDuringAskPending(t, a, "after throwing edit")
	})

	write(initJsHotReloadDuringAskGood)
	initJsHotReloadDuringAskPhase(t, a, "answer 2", func(t *testing.T) {
		a.typeText("2")
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(s string) bool {
			return strings.Contains(s, "❯? Pick a color → vermilion") && strings.Contains(s, "Color locked in.")
		}, "the answered ask and the next tape reply")
		if !a.waitDone(1, 20*time.Second) {
			t.Fatalf("turn never recorded done:\n%s", a.text())
		}
		if got := askDuringSessionPickerEntries(a, "ask/answer"); len(got) != 1 || got[0] != "vermilion" {
			t.Errorf("ask/answer entries = %q, want [vermilion]", got)
		}
		a.check("after answer")
	})

	initJsHotReloadDuringAskPhase(t, a, "new command works", func(t *testing.T) {
		time.Sleep(1500 * time.Millisecond) // past the watcher debounce of the last write
		a.typeText("/hotask-cmd z")
		a.key(uv.KeyEnter, 0)
		a.waitFor("HOTASK_CMD z")
	})

	initJsHotReloadDuringAskPhase(t, a, "rebound key active", func(t *testing.T) {
		a.typeText("draft to clear")
		a.waitFor("> draft to clear")
		a.key('g', uv.ModCtrl)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "draft to clear") },
			"ctrl+g (clear_input rebound by the reloaded init.js) to clear the composer")
	})
}
