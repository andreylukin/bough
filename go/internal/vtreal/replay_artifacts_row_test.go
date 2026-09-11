package vtreal

// A tape whose block called tools.artifact, replayed with the web row
// disabled: the recorded URL must reach the transcript, the turn must
// finish, and the next turn must too. The replay codemode answers the
// block from the tape, so neither row is needed for the transcript.
// The artifacts row cannot mount without web (the kernel refuses to
// boot: "missing [web]"), so both are disabled here and the boot
// refusal is pinned separately.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const artifactsRowURL = "localhost:7683/artifacts/rec/db-comparison"

const artifactsRowNoWeb = `
- id: web
  plugin: web
  disabled: true
`

func artifactsRowConfig(tape string) string {
	return replayConfig(tape) + artifactsRowNoWeb + `
- id: artifacts
  plugin: artifacts
  disabled: true
`
}

// Artifacts enabled with web disabled: boot must say which row is
// missing instead of hanging or crashing.
func TestArtifactsRowWithoutWebNamesMissingRow(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/artifacts-row.jsonl")
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	yml := replayConfig(tape) + artifactsRowNoWeb + "- id: artifacts\n  plugin: artifacts\n  config: {open: false}\n"
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(), "HOME="+home, "TERM=xterm-256color")
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, `row "artifacts" (artifacts) missing [web]`)
	}, "boot error naming the missing web row")
	if s := a.text(); panicky.MatchString(s) {
		t.Errorf("crash text instead of a clean boot refusal:\n%s", s)
	}
}

func TestArtifactsRow(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/artifacts-row.jsonl")
	a := startCfg(t, 100, 30, artifactsRowConfig(tape))
	a.check("boot")

	a.typeText("publish a db comparison page")
	a.key(uv.KeyEnter, 0)

	t.Run("TestArtifactsRowTurnFinishesWithoutWeb", func(t *testing.T) {
		if !a.waitDone(1, 30*time.Second) {
			t.Fatalf("artifact turn never finished:\n%s", a.text())
		}
		a.check("artifact turn")
	})
	t.Run("TestArtifactsRowLinkRenders", func(t *testing.T) {
		a.waitUntil(func(s string) bool { return strings.Contains(s, artifactsRowURL) }, "artifact URL on screen")
		if s := a.settled(); !strings.Contains(s, "Published:") {
			t.Errorf("stop text with the artifact link missing:\n%s", s)
		}
	})
	t.Run("TestArtifactsRowNoStoreWrite", func(t *testing.T) {
		// The transcript came from the tape: nothing was published.
		if _, err := os.Stat(filepath.Join(a.home, ".bough", "artifacts")); err == nil {
			t.Errorf("artifact store written though the block was replayed:\n%s", a.text())
		}
	})
	t.Run("TestArtifactsRowNextTurnNotBlocked", func(t *testing.T) {
		a.typeText("thanks")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(2, 30*time.Second) {
			t.Fatalf("turn after the artifact never finished:\n%s", a.text())
		}
		a.waitUntil(func(s string) bool { return strings.Contains(s, "Any time.") }, "second reply")
		a.check("after artifact")
	})
}
