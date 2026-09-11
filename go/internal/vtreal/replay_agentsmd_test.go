package vtreal

// AGENTS.md / CLAUDE.md context (plugins/contextmd) through the real
// binary. bough runs with cwd = the test's $HOME, so the files are
// written there. The echo llm answers "SYSTEM!" with the assembled
// system prompt, which the history records untruncated.
//
// Not covered: a subdirectory AGENTS.md picked up on cd. context-md
// reads "AGENTS.md"/"CLAUDE.md" relative to the process cwd, which
// never changes (a bash block's cd is its own subprocess); there is no
// cwd-change seam to drive.

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

const (
	agentsMdShared = "## Testing\n\nRun AGENTSMD_SHARED before pushing.\n"
	agentsMdA      = "# House\n\nAGENTSMD_ONLY_A is the rule.\n\n" + agentsMdShared
	// The same section reformatted (trailing spaces, extra blank line)
	// plus one of its own.
	agentsMdC = "## Testing   \n\n\nRun AGENTSMD_SHARED before pushing.\n\n## Extra\n\nAGENTSMD_ONLY_C too.\n"
)

func agentsMdWrite(t *testing.T, a *app, name, body string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(a.home, name), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// agentsMdSystem sends SYSTEM!, waits for the turn and returns the
// prompt the model was given (the last assistant entry).
func agentsMdSystem(t *testing.T, a *app, turn int) string {
	t.Helper()
	a.typeText("SYSTEM!")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(turn, 30*time.Second) {
		t.Fatalf("turn %d never finished:\n%s", turn, a.text())
	}
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	last, n := "", 0
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if s, _ := e.Data["text"].(string); e.Kind == "assistant" && strings.Contains(s, "Available in the runtime") {
				last = s
				n++
			}
		}
	}
	if n < turn {
		t.Fatalf("turn %d: %d SYSTEM! replies in history:\n%s", turn, n, a.text())
	}
	return last
}

// agentsMdStart writes the files before the first turn, which reads them.
func agentsMdStart(t *testing.T, cols int, files map[string]string) *app {
	t.Helper()
	a := start(t, cols, 30)
	for name, body := range files {
		agentsMdWrite(t, a, name, body)
	}
	return a
}

func TestAgentsMdDedupesSharedSection(t *testing.T) {
	t.Parallel()
	a := agentsMdStart(t, 100, map[string]string{"AGENTS.md": agentsMdA, "CLAUDE.md": agentsMdC})
	sys := agentsMdSystem(t, a, 1)
	if n := strings.Count(sys, "AGENTSMD_SHARED"); n != 1 {
		t.Errorf("shared section reached the prompt %d times, want 1:\n%s\n--- screen:\n%s", n, sys, a.text())
	}
	for _, want := range []string{"AGENTSMD_ONLY_A", "AGENTSMD_ONLY_C", "# Context: AGENTS.md", "# Context: CLAUDE.md"} {
		if !strings.Contains(sys, want) {
			t.Errorf("prompt lost %q:\n%s\n--- screen:\n%s", want, sys, a.text())
		}
	}
	if strings.Index(sys, "AGENTSMD_ONLY_A") > strings.Index(sys, "AGENTSMD_ONLY_C") {
		t.Errorf("AGENTS.md must come before CLAUDE.md:\n%s\n--- screen:\n%s", sys, a.text())
	}
	a.check("after SYSTEM!")
}

// A CLAUDE.md that is a pure copy contributes nothing: no label, no body.
func TestAgentsMdPureCopyVanishes(t *testing.T) {
	t.Parallel()
	a := agentsMdStart(t, 100, map[string]string{"AGENTS.md": agentsMdA, "CLAUDE.md": agentsMdA})
	sys := agentsMdSystem(t, a, 1)
	if strings.Contains(sys, "# Context: CLAUDE.md") || strings.Count(sys, "AGENTSMD_ONLY_A") != 1 {
		t.Errorf("a duplicate CLAUDE.md should vanish:\n%s\n--- screen:\n%s", sys, a.text())
	}
}

// The collapsed "▸ context: N pieces" block counts environment, both
// files and the base prompt at least, and opens to name the dropped section.
func TestAgentsMdContextBlockCountsPieces(t *testing.T) {
	t.Parallel()
	a := agentsMdStart(t, 200, map[string]string{"AGENTS.md": agentsMdA, "CLAUDE.md": agentsMdC})
	agentsMdSystem(t, a, 1)
	// The SYSTEM! reply is long: wheel up until the header is on screen.
	s, row, n := "", -1, 0
	for range 60 {
		s = a.settled()
		for i, l := range strings.Split(s, "\n") {
			if j := strings.Index(l, "context: "); j >= 0 && strings.Contains(l, "▸") && strings.Contains(l, " pieces") {
				row = i
				if _, err := fmt.Sscanf(l[j:], "context: %d pieces", &n); err != nil {
					t.Fatalf("unparseable context header %q:\n%s", l, s)
				}
				break
			}
		}
		if row >= 0 {
			break
		}
		a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
	}
	if row < 0 {
		t.Fatalf("no collapsed ▸ context block:\n%s", s)
	}
	if n < 4 {
		t.Errorf("context block counts %d pieces, want >= 4 (environment, AGENTS.md, CLAUDE.md, base):\n%s", n, s)
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "1 section(s) already in AGENTS.md, dropped")
	}, "opened context block names the dropped section")
	a.check("context opened")
}

// Files are read fresh each turn: an AGENTS.md created mid-session is
// in the next prompt, and still deduped against CLAUDE.md.
func TestAgentsMdCreatedMidSessionIsSeen(t *testing.T) {
	t.Parallel()
	a := agentsMdStart(t, 100, map[string]string{"CLAUDE.md": agentsMdC})
	if sys := agentsMdSystem(t, a, 1); strings.Contains(sys, "AGENTSMD_ONLY_A") {
		t.Fatalf("AGENTS.md content before the file exists:\n%s\n--- screen:\n%s", sys, a.text())
	}
	agentsMdWrite(t, a, "AGENTS.md", agentsMdA)
	sys := agentsMdSystem(t, a, 2)
	if !strings.Contains(sys, "AGENTSMD_ONLY_A") || strings.Count(sys, "AGENTSMD_SHARED") != 1 {
		t.Errorf("new AGENTS.md not picked up (or shared section doubled):\n%s\n--- screen:\n%s", sys, a.text())
	}
}
