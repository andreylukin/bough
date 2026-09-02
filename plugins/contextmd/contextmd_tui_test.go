// TUI-integration tests: a real contextmd.SystemContext (pointed at
// per-test temp files, so no HOME/cwd coupling) provided as the
// "context-md" service, with a stub provider that inspects its system
// prompt — pinning the contextmd -> loop -> llm -> renderer seam.
package contextmd_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/contextmd"
	"github.com/andreylukin/bough/plugins/llm"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/loop"
)

// sysDetector replies with which of the markers its system prompt contains.
func sysDetector(markers ...string) uitest.LLMFunc {
	return func(system string, _ []llm.Message) string {
		var seen []string
		for _, m := range markers {
			if strings.Contains(system, m) {
				seen = append(seen, m)
			}
		}
		if len(seen) == 0 {
			return "sysmark: none"
		}
		return "sysmark: " + strings.Join(seen, ",")
	}
}

// An AGENTS.md-style file's content reaches the system prompt at
// session start, proven end to end in the rendered reply.
func TestPreambleReachesSystemPrompt(t *testing.T) {
	t.Parallel()
	p := filepath.Join(t.TempDir(), "AGENTS.md")
	if err := os.WriteFile(p, []byte("Always obey CTXMARK_A1."), 0o644); err != nil {
		t.Fatal(err)
	}
	d := uitest.Mount(t, func(c *kernel.Context) {
		c.Provide("context-md", contextmd.New(p))
		c.Provide("llm", sysDetector("CTXMARK_A1"))
	}, "codemode", "loop")
	d.Say("hi")
	d.WaitFor("sysmark: CTXMARK_A1")
}

// Multiple context files land in order; missing ones are skipped
// without poisoning the prompt.
func TestMissingAndPresentFiles(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	present := filepath.Join(dir, "CLAUDE.md")
	if err := os.WriteFile(present, []byte("CTXMARK_B2 rules."), 0o644); err != nil {
		t.Fatal(err)
	}
	missing := filepath.Join(dir, "nope.md")
	d := uitest.Mount(t, func(c *kernel.Context) {
		c.Provide("context-md", contextmd.New(missing, present))
		c.Provide("llm", sysDetector("CTXMARK_B2", "nope.md"))
	}, "codemode", "loop")
	d.Say("hi")
	d.WaitFor("sysmark: CTXMARK_B2")
	if strings.Contains(d.Frame(), "nope.md") {
		t.Fatalf("missing file leaked into system prompt:\n%s", d.Frame())
	}
}
