package vtreal

// A timed input that fires while tools.ask is blocking the turn. The Go
// loop has no schedule of its own (the Rust schedule-cron went with the
// rebuild); the one timer-driven input it has is a background job's
// notice, so that stands in: the block detaches `sleep 1; echo TICK`
// and then asks. The job finishes while the ask is pending. It must
// be queued, not taken as the ask's answer, land exactly once after
// the ask resolves (landJobs before the next step), and never open a
// second wake turn.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const loopschedulefireswhileaskpendingCmd = "sleep 1; echo TICK-FIRED"

// loopschedulefireswhileaskpendingTape writes the one-turn tape: a
// block that starts the timed job and then asks, then a stop reply.
func loopschedulefireswhileaskpendingTape(t *testing.T) string {
	t.Helper()
	cmd, _ := json.Marshal(loopschedulefireswhileaskpendingCmd)
	code := "console.log(tools.bash(" + string(cmd) + ", 120));\n" +
		"console.log(\"answer=\" + tools.ask(\"Pick a color\", \"chartreuse\", \"vermilion\"));\n"
	rows := []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "tick then ask"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"code", map[string]any{"text": code}},
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
	p := filepath.Join(t.TempDir(), "tick-ask.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestLoopScheduleFiresWhileAskPending(t *testing.T) {
	t.Parallel()
	tape := loopschedulefireswhileaskpendingTape(t)
	cfg := jobsConfig(tape) + "- id: ask\n  plugin: ask\n  config: {timeout_minutes: 1}\n"
	a := startCfg(t, 100, 30, cfg)
	a.check("boot")
	jobsSay(a, "tick then ask")
	a.waitFor("? Pick a color")
	time.Sleep(3 * time.Second) // the 1s timer fires with the ask pending

	t.Run("pending ask untouched by the fire", func(t *testing.T) {
		s := a.settled()
		if !strings.Contains(s, "waiting for you") || !strings.Contains(s, askPendingPlaceholder) {
			t.Errorf("ask no longer pending after the timer fired:\n%s", s)
		}
		for _, e := range jobsHistory(a) {
			text, _ := e.Data["text"].(string)
			switch {
			case e.Kind == "ask/answer" || e.Kind == "done":
				t.Errorf("timer fire produced %s %v while the ask was pending", e.Kind, e.Data)
			case e.Kind == "input" && text != "tick then ask":
				t.Errorf("timer fire injected an input while the ask was pending: %q", text)
			}
		}
	})

	a.typeText("2")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished after answering:\n%s", a.text())
	}
	a.waitFor("Color locked in.")
	time.Sleep(2 * time.Second) // room for a stray wake turn

	es := jobsHistory(a)
	t.Run("ask answered with the user's pick", func(t *testing.T) {
		var answers []string
		for _, e := range es {
			if e.Kind == "ask/answer" {
				text, _ := e.Data["text"].(string)
				answers = append(answers, text)
			}
		}
		if len(answers) != 1 || answers[0] != "vermilion" {
			t.Errorf("want one ask answer %q, got %q", "vermilion", answers)
		}
	})

	t.Run("fires once after the ask resolves", func(t *testing.T) {
		ans, fires, first := -1, 0, -1
		for i, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "ask/answer" && ans < 0 {
				ans = i
			}
			if (e.Kind == "job" || e.Kind == "input") && strings.Contains(text, "TICK-FIRED") {
				fires++
				if first < 0 {
					first = i
				}
			}
		}
		if fires != 1 {
			t.Errorf("want the timed notice exactly once, got %d", fires)
		}
		if first >= 0 && first < ans {
			t.Errorf("timed notice at entry %d landed before the ask answer at %d", first, ans)
		}
	})

	t.Run("no double fire", func(t *testing.T) {
		if n := a.doneCount(); n != 1 {
			t.Errorf("want 1 finished turn (no wake turn), got %d:\n%s", n, a.text())
		}
		for _, e := range es {
			if text, _ := e.Data["text"].(string); e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
				t.Errorf("fire opened a wake turn: %q", text)
			}
		}
	})
}
