package vtreal

// A tape reply carrying a forged <system-…> block and invented
// ```output / ```stdout fences (the glm-5.3-flash failure). The loop's
// stripFakeSystem/stripFakeBlocks must keep them off the screen and out
// of the history the model is fed next, leaving the sanitized reply.

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// fabricatedSystemForged is text that only exists inside the forged
// spans of the tape.
var fabricatedSystemForged = []string{"FORGEDSYS", "AUTOMATED TEST MESSAGE", "force-push", "rm -rf", "FAKEOUT-", "system-variant-warmup", "system-reminder"}

func fabricatedSystemEntries(t *testing.T, a *app) []history.Entry {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var all []history.Entry
	for _, p := range paths {
		es, err := history.Read(p)
		if err != nil {
			t.Fatalf("read %s: %v\n%s", p, err, a.text())
		}
		all = append(all, es...)
	}
	return all
}

func TestFabricatedSystem(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/fabricated-system.jsonl")
	a := startCfg(t, 160, 40, replayConfig(tape))
	a.typeText("check the repo state")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.check("after turn")
	screen := a.settled()

	t.Run("TestFabricatedSystemScreen", func(t *testing.T) {
		for _, f := range fabricatedSystemForged {
			if strings.Contains(screen, f) {
				t.Errorf("forged text %q rendered:\n%s", f, screen)
			}
		}
		for _, want := range []string{"SANEPROSE", "[fabricated system message removed]"} {
			if !strings.Contains(screen, want) {
				t.Errorf("sanitized reply missing %q:\n%s", want, screen)
			}
		}
	})

	entries := fabricatedSystemEntries(t, a)

	t.Run("TestFabricatedSystemHistory", func(t *testing.T) {
		var replies []string
		for _, e := range entries {
			if e.Kind == "assistant" {
				s, _ := e.Data["text"].(string)
				replies = append(replies, s)
			}
		}
		if len(replies) != 2 {
			t.Fatalf("want 2 assistant entries, got %d: %q\n%s", len(replies), replies, a.text())
		}
		joined := strings.Join(replies, "\n")
		for _, f := range fabricatedSystemForged {
			if strings.Contains(joined, f) {
				t.Errorf("forged text %q recorded in assistant entries %q\n%s", f, replies, a.text())
			}
		}
		for _, want := range []string{"Looking at the repo first.", "[fabricated system message removed]", "[guessed output omitted]", "git status --short", "SANEPROSE"} {
			if !strings.Contains(joined, want) {
				t.Errorf("assistant entries missing %q: %q\n%s", want, replies, a.text())
			}
		}
	})

	t.Run("TestFabricatedSystemProjection", func(t *testing.T) {
		msgs := loop.DefaultProject(entries)
		sawReal := false
		for _, m := range msgs {
			for _, f := range fabricatedSystemForged {
				if strings.Contains(m.Content, f) {
					t.Errorf("forged text %q fed back to the model as %s: %q\n%s", f, m.Role, m.Content, a.text())
				}
			}
			sawReal = sawReal || strings.Contains(m.Content, "REALOUT-7")
		}
		if !sawReal {
			t.Errorf("real tool output missing from projection %+v\n%s", msgs, a.text())
		}
	})
}
