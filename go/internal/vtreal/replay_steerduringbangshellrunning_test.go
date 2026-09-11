package vtreal

// A steer typed while a "!" shell runs: `!sleep 3.<unique>; echo done`
// is started, a steer line is typed and submitted mid-run, then esc.
// Whatever esc cancels, nothing may be silently lost: the shell box
// shows its output or a cancelled marker, the steer text is in history
// exactly once, the composer is empty and no sleep is left behind
// (pgrep on the unique sleep argument). Replay-backed on the one-reply
// bang tape.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// steerDuringBangShellRunningMarker is a unique sleep duration: pgrep
// finds exactly this run's child by it.
func steerDuringBangShellRunningMarker() string {
	return fmt.Sprintf("3.%d%09d", os.Getpid()%1000, time.Now().Nanosecond())
}

func steerDuringBangShellRunningAlive(marker string) bool {
	return exec.Command("pgrep", "-f", "sleep "+marker).Run() == nil
}

// steerDuringBangShellRunningCount counts history entries (any kind)
// whose data text carries s.
func steerDuringBangShellRunningCount(a *app, s string) (int, []string) {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	n := 0
	var kinds []string
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			kinds = append(kinds, e.Kind)
			if txt, _ := e.Data["text"].(string); strings.Contains(txt, s) {
				n++
			}
		}
	}
	return n, kinds
}

func TestSteerDuringBangShellRunning(t *testing.T) {
	t.Parallel()
	t.Run("TestSteerDuringBangShellRunningEsc", func(t *testing.T) {
		t.Parallel()
		tape, _ := filepath.Abs("testdata/replay/bang-shell.jsonl")
		a := startCfg(t, 100, 30, replayConfig(tape))
		marker := steerDuringBangShellRunningMarker()
		t.Cleanup(func() { _ = exec.Command("pkill", "-f", "sleep "+marker).Run() })
		a.typeText("!sleep " + marker + "; echo sdbsr-done")
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return steerDuringBangShellRunningAlive(marker) }, "sleep child to start")

		steer := "sdbsr-steer-please-note"
		a.typeText(steer)
		a.key(uv.KeyEnter, 0)
		time.Sleep(300 * time.Millisecond)
		a.key(uv.KeyEscape, 0)

		deadline := time.Now().Add(8 * time.Second)
		for steerDuringBangShellRunningAlive(marker) && time.Now().Before(deadline) {
			time.Sleep(50 * time.Millisecond)
		}
		if steerDuringBangShellRunningAlive(marker) {
			t.Errorf("orphan sleep %s still running after esc + 8s:\n%s", marker, a.text())
		}
		a.waitUntil(func(s string) bool {
			return strings.Contains(s, "sdbsr-done") || strings.Contains(strings.ToLower(s), "cancel")
		}, "shell output or a cancelled marker")
		s := a.settled()
		a.check("after steer + esc")
		ls := strings.Split(s, "\n")
		if r := composerRow(ls); r < 0 || strings.TrimSpace(strings.TrimPrefix(ls[r], "> ")) != "" && !strings.Contains(ls[r], "say something") {
			t.Errorf("composer not empty after steer submit:\n%s", s)
		}
		time.Sleep(500 * time.Millisecond) // let a started turn log its input
		if n, kinds := steerDuringBangShellRunningCount(a, steer); n != 1 {
			t.Errorf("steer text in history %d times, want exactly 1\nkinds=%v\n%s", n, kinds, a.text())
		}
	})
}
