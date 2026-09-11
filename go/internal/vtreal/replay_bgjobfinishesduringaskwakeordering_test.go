package vtreal

// A background job started in turn 1 finishes while turn 2 is blocked
// on tools.ask. The notice must not open a wake turn, nor be taken as
// the answer, nor be drawn inside the pending ask card. It is delivered
// exactly once, after the answer: turn 2 has a block, so landJobs lands
// it before the next step and the idle wake then finds nothing pending.
// The tape has exactly four model replies; an extra call (a stray wake)
// would read "[replay: end of tape]".

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// bgjobfinishesduringaskwakeorderingTape writes the two-turn tape for a
// job that touches a sentinel when it finishes.
func bgjobfinishesduringaskwakeorderingTape(t *testing.T, cmd string) string {
	t.Helper()
	q, _ := json.Marshal(cmd)
	code1 := "console.log(tools.bash(" + string(q) + ", 120))\n"
	code2 := "console.log(\"answer=\" + tools.ask(\"Pick a color\", \"chartreuse\", \"vermilion\"));\n"
	rows := []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "start the job"}},
		{"assistant", map[string]any{"text": "```js\n" + code1 + "```"}},
		{"code", map[string]any{"text": code1}},
		{"assistant", map[string]any{"text": "```stop\nStarted job 1.\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": "now ask me"}},
		{"assistant", map[string]any{"text": "```js\n" + code2 + "```"}},
		{"code", map[string]any{"text": code2}},
		{"assistant", map[string]any{"text": "```stop\nColor locked in.\n```"}},
		{"done", map[string]any{"text": ""}},
	}
	var b strings.Builder
	for i, r := range rows {
		line, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": r.kind, "data": r.data})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "bgjob-ask.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestBgjobFinishesDuringAskWakeOrdering(t *testing.T) {
	t.Parallel()
	dir, err := os.MkdirTemp("", "bgask") // short: the path rides in the command
	if err != nil {
		t.Fatal(err)
	}
	sentinel := filepath.Join(dir, "done")
	t.Cleanup(func() { os.Remove(sentinel); os.Remove(dir) })
	cmd := "sleep 1; touch " + sentinel + "; echo BGASK-SENTINEL"
	tape := bgjobfinishesduringaskwakeorderingTape(t, cmd)
	cfg := jobsConfig(tape) + "- id: ask\n  plugin: ask\n  config: {timeout_minutes: 1}\n"
	a := startCfg(t, 100, 30, cfg)
	a.check("boot")

	jobsSay(a, "start the job")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	jobsSay(a, "now ask me")
	a.waitFor("? Pick a color")
	if _, err := os.Stat(sentinel); err == nil {
		t.Fatalf("job finished before the ask was pending; not a finish-during-ask")
	}
	deadline := time.Now().Add(30 * time.Second)
	for {
		if _, err := os.Stat(sentinel); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("the background job never finished:\n%s", a.text())
		}
		time.Sleep(50 * time.Millisecond)
	}
	time.Sleep(1500 * time.Millisecond) // room for the notice (and any stray wake) to land

	t.Run("no wake while the ask is pending", func(t *testing.T) {
		s := a.settled()
		if !strings.Contains(s, "waiting for you") || !strings.Contains(s, askPendingPlaceholder) {
			t.Errorf("ask no longer pending after the job finished:\n%s", s)
		}
		if n := a.doneCount(); n != 1 {
			t.Errorf("want 1 finished turn while the ask is pending, got %d", n)
		}
		for _, e := range jobsHistory(a) {
			text, _ := e.Data["text"].(string)
			switch {
			case e.Kind == "ask/answer" || e.Kind == "job":
				t.Errorf("job finish produced %s %q while the ask was pending", e.Kind, text)
			case e.Kind == "input" && text != "start the job" && text != "now ask me":
				t.Errorf("job finish injected an input while the ask was pending: %q", text)
			}
		}
	})

	t.Run("notice not inside the ask card", func(t *testing.T) {
		ls := a.lines()
		top, bot := -1, -1
		for i, l := range ls {
			if top < 0 && strings.Contains(l, "? Pick a color") {
				top = i
			}
			if top >= 0 && strings.Contains(l, "2.") && strings.Contains(l, "vermilion") {
				bot = i
			}
		}
		if top < 0 || bot < 0 {
			t.Fatalf("ask card not found:\n%s", a.text())
		}
		for _, l := range ls[top : bot+1] {
			if strings.Contains(l, "BGASK-SENTINEL") || strings.Contains(l, "[background job]") || strings.Contains(l, "job 1") {
				t.Errorf("job notice drawn inside the ask card: %q\n%s", l, a.text())
			}
		}
	})

	a.typeText("2")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("turn 2 never finished after answering:\n%s", a.text())
	}
	a.waitFor("Color locked in.")
	time.Sleep(2 * time.Second) // room for a stray wake turn

	es := jobsHistory(a)
	t.Run("delivered exactly once after the answer", func(t *testing.T) {
		ans, first, n := -1, -1, 0
		for i, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "ask/answer" && ans < 0 {
				ans = i
			}
			if (e.Kind == "job" || e.Kind == "input") && strings.Contains(text, "BGASK-SENTINEL") {
				n++
				if first < 0 {
					first = i
				}
			}
		}
		if n != 1 {
			t.Errorf("want the job notice delivered exactly once, got %d", n)
		}
		if ans < 0 || first < ans {
			t.Errorf("job notice at entry %d is not after the ask answer at %d", first, ans)
		}
	})

	t.Run("tape call count", func(t *testing.T) {
		calls := 0
		for _, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "assistant" {
				calls++
				if strings.Contains(text, "end of tape") {
					t.Errorf("an extra model call ran past the tape: %q", text)
				}
			}
			if e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
				t.Errorf("job opened a separate wake turn: %q", text)
			}
		}
		if calls != 4 {
			t.Errorf("want 4 model calls (the whole tape), got %d", calls)
		}
		if n := a.doneCount(); n != 2 {
			t.Errorf("want 2 finished turns, got %d:\n%s", n, a.text())
		}
		if s := a.settled(); strings.Contains(s, "[background job]") || strings.Contains(s, "end of tape") {
			t.Errorf("wake preamble or tape overrun on screen:\n%s", s)
		}
	})
}
