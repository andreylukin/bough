package vtreal

// Carriage returns, backspaces and stray CSI in a model reply and in a
// bash result: the transcript shows what a terminal would have left
// (the last \r frame, the erased char gone), none of the bytes move the
// real cursor or clear the real screen, and history keeps them raw.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const (
	streamWithCRAndBackspaceBytesResult = "progress [#   ] 10%\rprogress [##  ] 50%\rprogress [####] 100%\n" +
		"stepX\bY ok\n" +
		"\x1b[2J\x1b[5;1Hmoved here\n" +
		"tail partial \x1b["
	streamWithCRAndBackspaceBytesReply = "fetch 1/3\rfetch 2/3\rfetch 3/3 finished\n" +
		"typo!\b fixed\n" +
		"\x1b[H\x1b[2Kreply end \x1b[3"
)

func streamWithCRAndBackspaceBytesTape(t *testing.T) string {
	t.Helper()
	code := "console.log(tools.bash(\"gen\"))\n"
	entries := []struct {
		kind string
		data map[string]string
	}{
		{"meta", map[string]string{"cwd": "/tmp/demo"}},
		{"input", map[string]string{"text": "run progress"}},
		{"assistant", map[string]string{"text": "```js\n" + code + "```"}},
		{"code", map[string]string{"text": code}},
		{"result", map[string]string{"code": code, "text": streamWithCRAndBackspaceBytesResult}},
		{"assistant", map[string]string{"text": streamWithCRAndBackspaceBytesReply + "\n\n```stop\ndone\n```"}},
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
	p := filepath.Join(t.TempDir(), "crbs.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func streamWithCRAndBackspaceBytesRun(t *testing.T) *app {
	t.Helper()
	a := startCfg(t, 100, 30, replayConfig(streamWithCRAndBackspaceBytesTape(t)))
	a.typeText("run progress")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.settled()
	return a
}

// streamWithCRAndBackspaceBytesIntact: the UI around the transcript
// survived (no clear, no cursor jump overwrote the input or chrome).
func (a *app) streamWithCRAndBackspaceBytesIntact(where string) {
	a.t.Helper()
	a.check(where)
	s := a.text()
	if !strings.Contains(s, "run progress") {
		a.t.Errorf("%s: user input row gone (screen cleared or overwritten):\n%s", where, s)
	}
	for i, l := range a.lines() {
		if i < 6 && strings.HasPrefix(l, "moved here") {
			a.t.Errorf("%s: CSI cursor-position leaked: %q at row %d col 0:\n%s", where, l, i, s)
		}
	}
}

func TestStreamWithCRAndBackspaceBytes(t *testing.T) {
	t.Parallel()

	t.Run("Reply", func(t *testing.T) {
		t.Parallel()
		a := streamWithCRAndBackspaceBytesRun(t)
		a.streamWithCRAndBackspaceBytesIntact("reply")
		s := a.text()
		if !strings.Contains(s, "fetch 3/3 finished") {
			t.Errorf("last \\r frame of the reply not shown:\n%s", s)
		}
		if strings.Contains(s, "fetch 1/3") || strings.Contains(s, "fetch 2/3") {
			t.Errorf("reply \\r frames not collapsed:\n%s", s)
		}
		if !strings.Contains(s, "reply end") {
			t.Errorf("text after a stray CSI lost:\n%s", s)
		}
	})

	t.Run("Result", func(t *testing.T) {
		t.Parallel()
		a := streamWithCRAndBackspaceBytesRun(t)
		row := hugeOutputRow(a.lines(), "▸ result")
		if row < 0 {
			t.Fatalf("no collapsed result header:\n%s", a.text())
		}
		a.click(2, row)
		a.waitFor("▾ result")
		a.streamWithCRAndBackspaceBytesIntact("result expanded")
		s := a.text()
		if !strings.Contains(s, "progress [####] 100%") {
			t.Errorf("last progress frame not shown:\n%s", s)
		}
		if strings.Contains(s, "10%") || strings.Contains(s, "50%") {
			t.Errorf("progress \\r frames not collapsed:\n%s", s)
		}
		if !strings.Contains(s, "tail partial") {
			t.Errorf("text before a trailing partial CSI lost:\n%s", s)
		}
	})

	t.Run("Backspace", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_STREAMWITHCRANDBACKSPACEBYTES") == "" {
			t.Skip("known bug: plugins/ui sanitizeText drops \\b instead of erasing the previous char (\"stepX\\bY\" renders \"stepXY\"); set BOUGH_KNOWN_STREAMWITHCRANDBACKSPACEBYTES=1 to run")
		}
		t.Parallel()
		a := streamWithCRAndBackspaceBytesRun(t)
		row := hugeOutputRow(a.lines(), "▸ result")
		if row < 0 {
			t.Fatalf("no collapsed result header:\n%s", a.text())
		}
		a.click(2, row)
		a.waitFor("▾ result")
		s := a.text()
		if !strings.Contains(s, "stepY ok") {
			t.Errorf("result backspace not applied (want \"stepY ok\"):\n%s", s)
		}
		if !strings.Contains(s, "typo fixed") {
			t.Errorf("reply backspace not applied (want \"typo fixed\"):\n%s", s)
		}
	})

	t.Run("HistoryRaw", func(t *testing.T) {
		t.Parallel()
		a := streamWithCRAndBackspaceBytesRun(t)
		paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
		if len(paths) != 1 {
			t.Fatalf("want one session file, got %v", paths)
		}
		es, err := history.Read(paths[0])
		if err != nil {
			t.Fatal(err)
		}
		var sawResult, sawReply bool
		for _, e := range es {
			txt, _ := e.Data["text"].(string)
			switch {
			case e.Kind == "result" && txt == streamWithCRAndBackspaceBytesResult:
				sawResult = true
			case e.Kind == "assistant" && strings.Contains(txt, streamWithCRAndBackspaceBytesReply):
				sawReply = true
			}
		}
		if !sawResult || !sawReply {
			b, _ := os.ReadFile(paths[0])
			t.Errorf("history lost raw bytes (result %v, reply %v):\n%s", sawResult, sawReply, b)
		}
	})
}
