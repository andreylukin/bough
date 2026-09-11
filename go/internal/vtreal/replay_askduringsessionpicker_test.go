package vtreal

// ask-during-session-picker: the tape's block waits on a gate file,
// then calls tools.ask. The test opens the /sessions picker while the
// turn runs, opens the gate, and only once the ask is recorded closes
// the picker with esc. The ask must still be pending on screen (not
// lost, not answered by the picker's keys), and "1" must reach the
// block: the recorded result is "you picked chartreuse".

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

// askDuringSessionPickerTape writes a tape whose block blocks on gate
// before asking, and returns its path.
func askDuringSessionPickerTape(t *testing.T, gate string) string {
	t.Helper()
	code := fmt.Sprintf("tools.bash(%q);\nconst c = tools.ask(\"Pick a color\", \"chartreuse\", \"vermilion\");\nconsole.log(\"you picked \" + c);\n",
		fmt.Sprintf("while [ ! -f %s ]; do sleep 0.05; done", gate))
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
	p := filepath.Join(t.TempDir(), "ask_during_session_picker.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// askDuringSessionPickerEntries returns text+question of every entry of
// kind in this run's session files.
func askDuringSessionPickerEntries(a *app, kind string) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []string
	for _, p := range paths {
		b, err := os.ReadFile(p)
		if err != nil {
			continue
		}
		for _, l := range strings.Split(string(b), "\n") {
			var e struct {
				Kind string `json:"kind"`
				Data struct {
					Text     string `json:"text"`
					Question string `json:"question"`
				} `json:"data"`
			}
			if json.Unmarshal([]byte(l), &e) == nil && e.Kind == kind {
				out = append(out, e.Data.Text+e.Data.Question)
			}
		}
	}
	return out
}

func TestAskDuringSessionPicker(t *testing.T) {
	t.Parallel()
	gate := filepath.Join(t.TempDir(), "gate")
	a := startCfg(t, 100, 30, askConfig(askDuringSessionPickerTape(t, gate)))
	a.typeText("pick a color")
	a.key(uv.KeyEnter, 0)
	a.waitFor("pick a color")

	a.typeText("/sessions")
	a.waitFor("> /sessions")
	a.key(uv.KeyEnter, 0)
	a.waitFor("resume a session")

	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(15 * time.Second)
	for len(askDuringSessionPickerEntries(a, "ask")) == 0 {
		if time.Now().After(deadline) {
			t.Fatalf("ask never recorded:\n%s", a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.settled()
	if got := askDuringSessionPickerEntries(a, "ask/answer"); len(got) != 0 {
		t.Fatalf("ask answered while the picker was open: %q\n%s", got, a.text())
	}

	a.key(uv.KeyEscape, 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "resume a session") }, "the picker to close")
	s := a.settled()
	for _, want := range []string{"? Pick a color", "1.", "chartreuse", "2.", "vermilion", askPendingPlaceholder} {
		if !strings.Contains(s, want) {
			t.Errorf("pending ask missing %q after closing the picker:\n%s", want, s)
		}
	}
	if t.Failed() {
		return
	}

	a.typeText("1")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "❯? Pick a color → chartreuse") && strings.Contains(s, "Color locked in.")
	}, "the answered ask and the next tape reply")
	if !a.waitDone(1, 20*time.Second) {
		t.Fatalf("turn never recorded done:\n%s", a.text())
	}
	if got := askDuringSessionPickerEntries(a, "ask/answer"); len(got) != 1 || got[0] != "chartreuse" {
		t.Errorf("ask/answer entries = %q, want [chartreuse]", got)
	}
	res := askDuringSessionPickerEntries(a, "result")
	if len(res) != 1 || !strings.Contains(res[0], "you picked chartreuse") {
		t.Errorf("tool result = %q, want it to carry \"you picked chartreuse\"", res)
	}
	a.check("ask-during-session-picker end")
}
