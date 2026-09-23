//go:build !windows

package contextmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/unreal/prompt"
)

// engineContext is the Context part the engine row reads from this row
// at every build and every Submit.
func engineContext(s *SystemContext) []prompt.Part {
	var out []prompt.Part
	for _, p := range s.Parts() {
		out = append(out, prompt.Part{Name: p.Path, Text: p.Text})
	}
	return out
}

// On the engine, AGENTS.md and the project's MEMORY.md reach the model
// the way they reach the loop — MEMORY.md first, a section CLAUDE.md
// repeats dropped — but in the frozen prompt once, and afterwards only
// as reminders: an edit to AGENTS.md, or a MEMORY.md an agent writes
// mid-session, arrives whole at the next input.
func TestEngineContextFilesFreezeThenRemind(t *testing.T) {
	t.Parallel()
	home, work := t.TempDir(), t.TempDir()
	paths := Paths(home, "proj")
	for i, p := range paths {
		if !filepath.IsAbs(p) {
			paths[i] = filepath.Join(work, p)
		}
	}
	memory, agents, claude := paths[0], paths[1], paths[2]
	write := func(p, body string) {
		t.Helper()
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write(agents, "# Build\nmake all\n\n# Style\ntabs\n")
	write(claude, "# Style\ntabs\n")
	s := New(paths...)

	first := prompt.Parts{Preamble: prompt.Preamble(0), Context: engineContext(s)}
	sys := prompt.Compose(first)
	if !strings.Contains(sys, "# Context: "+agents+"\n# Build\nmake all") || strings.Contains(sys, "# Context: "+claude) {
		t.Fatalf("frozen prompt context:\n%s", sys)
	}
	if text, _ := prompt.Reminder(first, prompt.Parts{Context: engineContext(s)}); text != "" {
		t.Fatalf("unchanged files reminded:\n%s", text)
	}

	write(memory, "# Standing brief\nship on fridays\n")
	write(agents, "# Build\nmake all -j8\n\n# Style\ntabs\n")
	now := prompt.Parts{Context: engineContext(s)}
	text, changed := prompt.Reminder(first, now)
	if len(changed) != 2 || changed[0] != memory || changed[1] != agents {
		t.Fatalf("changed = %v", changed)
	}
	if !strings.Contains(text, "ship on fridays") || !strings.Contains(text, "make all -j8") || strings.Contains(text, "# Context: "+claude) {
		t.Fatalf("reminder:\n%s", text)
	}
	// The next diff is against what the model has now seen.
	if text, _ := prompt.Reminder(now, prompt.Parts{Context: engineContext(s)}); text != "" {
		t.Fatalf("a delivered change reminded again:\n%s", text)
	}
}
