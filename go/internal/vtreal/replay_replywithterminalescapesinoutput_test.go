package vtreal

// Tool output and model text carrying raw terminal escapes (clear
// screen, cursor moves, OSC 52 clipboard write, OSC 0 title, SGR).
// sanitizeText (plugins/ui/blocks.go) strips them by design, so the
// screen must keep its earlier content, the text around each escape
// must survive, the title must not change, and — in the pane's raw
// bytes captured via tmux pipe-pane — no injected OSC 52 or title may
// reach the terminal.

import (
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const replyWithTerminalEscapesInOutputTitle = "PWNTITLE"

var replyWithTerminalEscapesInOutputB64 = base64.StdEncoding.EncodeToString([]byte("PWNCLIP"))

// replyWithTerminalEscapesInOutputPayload wraps every escape between
// marker words tag+"A" ... tag+"Z".
func replyWithTerminalEscapesInOutputPayload(tag string) string {
	return tag + "A \x1b[2J\x1b[H" + tag + "B \x1b[12;7H" + tag + "C \x1b]52;c;" + replyWithTerminalEscapesInOutputB64 + "\x07" +
		tag + "D \x1b]0;" + replyWithTerminalEscapesInOutputTitle + "\x07" + tag + "E \x1b[31m" + tag + "Z\x1b[0m"
}

func replyWithTerminalEscapesInOutputTape(t *testing.T) string {
	t.Helper()
	code := "console.log(tools.bash(\"cat evil\"))\n"
	entries := []struct {
		kind string
		data map[string]string
	}{
		{"meta", map[string]string{"cwd": "/tmp/demo"}},
		{"input", map[string]string{"text": "show escapes"}},
		{"assistant", map[string]string{"text": "```js\n" + code + "```"}},
		{"code", map[string]string{"text": code}},
		{"result", map[string]string{"code": code, "text": replyWithTerminalEscapesInOutputPayload("TOOL") + "\n"}},
		{"assistant", map[string]string{"text": replyWithTerminalEscapesInOutputPayload("MODEL") + "\n\n```stop\ndone\n```"}},
		{"done", map[string]string{"text": ""}},
	}
	var sb strings.Builder
	for i, e := range entries {
		b, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-10T10:00:00Z", "kind": e.kind, "data": e.data})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "escapes.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// replyWithTerminalEscapesInOutputScreen checks one settled screen.
func replyWithTerminalEscapesInOutputScreen(t *testing.T, s, tag string) {
	t.Helper()
	if !strings.Contains(s, "show escapes") {
		t.Errorf("earlier transcript gone (screen cleared?):\n%s", s)
	}
	for _, m := range []string{"A", "B", "C", "D", "E", "Z"} {
		if !strings.Contains(s, tag+m) {
			t.Errorf("marker %s%s missing (text lost or moved off-row):\n%s", tag, m, s)
		}
	}
	// Cursor moves would scatter the markers; stripped, they stay on one row.
	row := ""
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, tag+"A") {
			row = l
		}
	}
	if !strings.Contains(row, tag+"B") {
		t.Errorf("%sB not on %sA's row (cursor move applied?): %q", tag, tag, row)
	}
	for _, bad := range []string{"\x1b", "[2J", "]52;", replyWithTerminalEscapesInOutputB64, replyWithTerminalEscapesInOutputTitle} {
		if strings.Contains(s, bad) {
			t.Errorf("escape residue %q on screen:\n%s", bad, s)
		}
	}
}

func TestReplyWithTerminalEscapesInOutput(t *testing.T) {
	t.Parallel()
	tape := replyWithTerminalEscapesInOutputTape(t)

	t.Run("vt_screen", func(t *testing.T) {
		t.Parallel()
		a := startCfg(t, 120, 40, replayConfig(tape))
		a.typeText("show escapes")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(1, 60*time.Second) {
			t.Fatalf("turn never finished:\n%s", a.text())
		}
		a.waitFor("MODELZ")
		a.check("after reply")
		s := a.settled()
		t.Run("model_text", func(t *testing.T) { replyWithTerminalEscapesInOutputScreen(t, s, "MODEL") })
		t.Run("tool_output", func(t *testing.T) {
			ls := a.lines()
			row := hugeOutputRow(ls, "▸ result")
			if row >= 0 {
				a.click(2, row)
				a.waitFor("TOOLA")
			}
			replyWithTerminalEscapesInOutputScreen(t, a.settled(), "TOOL")
		})
		t.Run("title", func(t *testing.T) {
			if got := a.term.Snapshot().Title; strings.Contains(got, replyWithTerminalEscapesInOutputTitle) {
				t.Errorf("injected OSC 0 set the window title: %q", got)
			}
		})
	})

	t.Run("raw_bytes", func(t *testing.T) {
		t.Parallel()
		tm, home := resizeTmuxStart(t, 120, 40, replayConfig(tape))
		raw := filepath.Join(t.TempDir(), "pane.raw")
		tm.run("pipe-pane", "-o", "-t", "0", "cat > "+raw)
		resizeTmuxSend(tm, "show escapes")
		resizeTmuxWaitDone(t, tm, home, 1)
		tm.waitFor("MODELZ")
		tm.settled()
		time.Sleep(300 * time.Millisecond)
		b, err := os.ReadFile(raw)
		if err != nil || len(b) == 0 {
			t.Fatalf("no raw bytes captured: %v", err)
		}
		out := string(b)
		if strings.Contains(out, "\x1b]52;c;"+replyWithTerminalEscapesInOutputB64) || strings.Contains(out, replyWithTerminalEscapesInOutputB64) {
			t.Errorf("injected OSC 52 clipboard write forwarded to the terminal")
		}
		if strings.Contains(out, replyWithTerminalEscapesInOutputTitle) {
			t.Errorf("injected OSC 0 title forwarded to the terminal")
		}
		for _, tag := range []string{"MODEL", "TOOL"} {
			if strings.Contains(out, tag+"A \x1b[2J") || strings.Contains(out, tag+"B \x1b[12;7H") {
				t.Errorf("injected %s CSI forwarded verbatim", tag)
			}
		}
	})
}
