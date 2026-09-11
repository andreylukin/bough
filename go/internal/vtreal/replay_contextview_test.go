package vtreal

// The context view: the per-turn "▸ context (N lines)" block names the
// pieces of the system prompt with their sizes, /context prints those
// pieces in full, and SYSTEM! (echo llm) is the cross-check that what
// /context shows is what the model got.

import (
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const contextViewMarker = "CTXVIEW-MARKER-7f3a"

var (
	contextViewHeader = regexp.MustCompile(`▸ context \((\d+) lines\): context: (\d+) pieces, (\d+) chars`)
	contextViewPiece  = regexp.MustCompile(`(?m)^- .+: \d+ lines?, \d+ chars$`)
)

// contextViewStart boots the echo config with an AGENTS.md in the
// working directory, so the prompt has more than environment + base.
func contextViewStart(t *testing.T) *app {
	t.Helper()
	a := start(t, 100, 30)
	// Written after boot: context files are read when a turn starts.
	if err := os.WriteFile(filepath.Join(a.home, "AGENTS.md"),
		[]byte("# Rules\n\n"+contextViewMarker+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return a
}

// contextViewWheelUntil scrolls (paced: unpaced wheel events fill the
// PTY pipe and block SendMouse) until the screen contains substr.
func contextViewWheelUntil(a *app, b uv.MouseButton, substr string) {
	a.t.Helper()
	for range 300 {
		if strings.Contains(a.text(), substr) {
			return
		}
		a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: b})
		time.Sleep(5 * time.Millisecond)
	}
	a.waitFor(substr) // fails with the screen
}

func TestContextViewTurnBlock(t *testing.T) {
	t.Parallel()
	a := contextViewStart(t)
	a.typeText("hello")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: hello")
	a.waitUntil(contextViewHeader.MatchString, "a collapsed context header")
	s := a.settled()
	m := contextViewHeader.FindStringSubmatch(s)
	lines, _ := strconv.Atoi(m[1])
	pieces, _ := strconv.Atoi(m[2])
	if pieces < 3 || lines != pieces+1 {
		t.Fatalf("header says %d lines for %d pieces (want pieces+1, pieces>=3):\n%s", lines, pieces, s)
	}
	row := -1
	for i, l := range strings.Split(s, "\n") {
		if contextViewHeader.MatchString(l) {
			row = i
		}
	}
	a.click(2, row)
	a.waitFor("- base prompt:") // open: the body replaces the header
	s = a.settled()
	for _, want := range []string{"- environment:", "- AGENTS.md", "context: " + m[2] + " pieces, " + m[3] + " chars"} {
		if !strings.Contains(s, want) {
			t.Errorf("expanded context block lacks %q:\n%s", want, s)
		}
	}
	if n := len(contextViewPiece.FindAllString(s, -1)); n != pieces {
		t.Errorf("expanded block lists %d sized pieces, header said %d:\n%s", n, pieces, s)
	}
	a.check("context expanded")
}

func TestContextViewSlashCommand(t *testing.T) {
	t.Parallel()
	a := contextViewStart(t)
	a.typeText("hello") // a turn first: /context then shows the snapshot it used
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: hello")

	a.typeText("/context")
	a.key(uv.KeyEnter, 0)
	a.waitFor("spawnAll") // the tail of the tools section lands at the bottom
	a.check("/context printed")

	t.Run("scrolls", func(t *testing.T) {
		contextViewWheelUntil(a, uv.MouseWheelUp, contextViewMarker)
		contextViewWheelUntil(a, uv.MouseWheelUp, "Everything the model is told before your message")
		s := a.settled()
		if !regexp.MustCompile(`## environment — \d+ lines?, \d+ chars`).MatchString(s) {
			t.Errorf("top of /context lacks the sized environment section:\n%s", s)
		}
		contextViewWheelUntil(a, uv.MouseWheelDown, "spawnAll")
		a.check("scrolled back")
	})

	t.Run("esc back", func(t *testing.T) {
		a.key(uv.KeyEscape, 0)
		a.check("after esc")
		a.typeText("again")
		a.key(uv.KeyEnter, 0)
		a.waitFor("echo: again")
	})

	t.Run("SYSTEM cross-check", func(t *testing.T) {
		before := a.doneCount()
		a.typeText("SYSTEM!")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(before+1, 30*time.Second) {
			t.Fatalf("SYSTEM! turn never finished:\n%s", a.text())
		}
		// The screen alone cannot tell the reply from /context's
		// output; the recorded reply is the prompt the model got.
		paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
		got := false
		for _, p := range paths {
			entries, _ := history.Read(p)
			for _, e := range entries {
				if txt, _ := e.Data["text"].(string); e.Kind == "assistant" && strings.Contains(txt, contextViewMarker) {
					got = true
				}
			}
		}
		if !got {
			t.Errorf("the prompt the model got lacks the AGENTS.md text /context showed:\n%s", a.text())
		}
		a.check("after SYSTEM!")
	})
}
