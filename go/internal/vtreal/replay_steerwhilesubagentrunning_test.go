package vtreal

// A steer typed while a spawned subagent is still working. Replay's
// codemode never spawns, so this runs the real codemode + workers rows
// and lets the replay llm answer both agents from one tape, in call
// order: parent spawns, child runs a slow bash block, child reports,
// parent answers. The steer, entered while the child sleeps, belongs
// to the parent: it lands after the spawn block returns, once, and the
// child's report still reaches the parent.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const (
	steerWhileSubagentRunningText   = "also check the staging logs"
	steerWhileSubagentRunningReport = "Status: ok\nFindings: SUBREPORT-7731 the build is green."
	steerWhileSubagentRunningAck    = "Checked staging too, as asked."
)

func steerWhileSubagentRunningTape(t *testing.T, dir string) string {
	t.Helper()
	entries := []map[string]any{
		{"kind": "input", "data": map[string]any{"text": "check the build"}},
		{"kind": "assistant", "data": map[string]any{"text": "Delegating.\n\n```js\nconsole.log(tools.spawn(\"wait for the build\"))\n```"}},
		// The child's two calls come from the same llm row, in order.
		{"kind": "assistant", "data": map[string]any{"text": "```js\nconsole.log(tools.bash(\"sleep 4 && echo SUB-SLEPT\"))\n```"}},
		{"kind": "assistant", "data": map[string]any{"text": steerWhileSubagentRunningReport}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\n" + steerWhileSubagentRunningAck + "\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = "2026-09-11T10:00:00Z"
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	path := filepath.Join(dir, "steer-sub.jsonl")
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// steerWhileSubagentRunningConfig: replay llm, real codemode, tools and
// workers; history optionally resumes a file.
func steerWhileSubagentRunningConfig(tape, resume string) string {
	hist := "- id: history\n  plugin: history\n"
	if resume != "" {
		hist = fmt.Sprintf("- id: history\n  plugin: history\n  config: {file: %q}\n", resume)
	}
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: codemode
  plugin: codemode
- id: commands
  plugin: commands
- id: tools
  plugin: tools-basic
- id: workers
  plugin: workers
%s- id: loop
  plugin: loop
- id: ui
  plugin: ui
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
`, tape, hist)
}

// steerWhileSubagentRunningSession is the one session file the run wrote.
func steerWhileSubagentRunningSession(t *testing.T, a *app) (string, []history.Entry) {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	if len(paths) != 1 {
		t.Fatalf("want one session file, got %v", paths)
	}
	es, err := history.Read(paths[0])
	if err != nil {
		t.Fatal(err)
	}
	return paths[0], es
}

func TestSteerWhileSubagentRunning(t *testing.T) {
	t.Parallel()
	tape := steerWhileSubagentRunningTape(t, t.TempDir())
	a := startCfg(t, 100, 30, steerWhileSubagentRunningConfig(tape, ""))

	a.typeText("check the build")
	a.key(uv.KeyEnter, 0)
	a.waitFor("subagent 1") // the card is up; the child sleeps in its block
	a.typeText(steerWhileSubagentRunningText)
	a.key(uv.KeyEnter, 0)
	a.waitFor(steerWhileSubagentRunningText + " (steer · pending)")

	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.check("after steered turn")
	screen := a.settled()
	path, es := steerWhileSubagentRunningSession(t, a)

	t.Run("SteerGoesToParent", func(t *testing.T) {
		for _, e := range es {
			if !strings.HasPrefix(e.Kind, "sub:") {
				continue
			}
			if b, _ := json.Marshal(e.Data); strings.Contains(string(b), steerWhileSubagentRunningText) {
				t.Errorf("steer leaked into the subagent: %s %s", e.Kind, b)
			}
		}
	})

	t.Run("SteerOnceInHistory", func(t *testing.T) {
		n := 0
		for _, e := range es {
			if text, _ := e.Data["text"].(string); e.Kind == "input" && text == steerWhileSubagentRunningText {
				n++
				if e.Data["steer"] != true {
					t.Errorf("steer input not marked steer: %v", e.Data)
				}
			}
		}
		if n != 1 {
			t.Errorf("steer inputs in history = %d, want 1", n)
		}
		if c := strings.Count(screen, steerWhileSubagentRunningText); c != 1 || strings.Contains(screen, "pending") {
			t.Errorf("steer on screen %d times (want 1, landed):\n%s", c, screen)
		}
	})

	t.Run("SubagentResultDelivered", func(t *testing.T) {
		var seen []string
		for _, e := range es {
			text, _ := e.Data["text"].(string)
			switch {
			case e.Kind == "result" && strings.Contains(text, "SUBREPORT-7731"):
				seen = append(seen, "report")
			case e.Kind == "input" && e.Data["steer"] == true:
				seen = append(seen, "steer")
			case e.Kind == "assistant" && strings.Contains(text, steerWhileSubagentRunningAck):
				seen = append(seen, "ack")
			case e.Kind == "sub:done":
				seen = append(seen, "subdone")
			}
		}
		if got := strings.Join(seen, " "); got != "subdone report steer ack" {
			t.Errorf("history order = %q, want \"subdone report steer ack\"\nscreen:\n%s", got, screen)
		}
		if !strings.Contains(screen, steerWhileSubagentRunningAck) {
			t.Errorf("parent's answer missing:\n%s", screen)
		}
		if n := a.doneCount(); n != 1 {
			t.Errorf("done entries = %d, want 1", n)
		}
	})

	t.Run("NoDuplicateUserBlockAfterResume", func(t *testing.T) {
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		cp := filepath.Join(t.TempDir(), "resume.jsonl")
		if err := os.WriteFile(cp, data, 0o644); err != nil {
			t.Fatal(err)
		}
		b := startCfg(t, 100, 40, steerWhileSubagentRunningConfig(tape, cp))
		b.waitFor(steerWhileSubagentRunningAck)
		b.check("resumed")
		s := b.settled()
		// User blocks only: the "resumed · last: …" line quotes the steer too.
		if n := strings.Count(s, "❯ "+steerWhileSubagentRunningText); n != 1 {
			t.Errorf("steer shown %d times after resume, want 1:\n%s", n, s)
		}
		if n := strings.Count(s, "❯ check the build"); n != 1 {
			t.Errorf("first input shown %d times after resume, want 1:\n%s", n, s)
		}
	})
}
