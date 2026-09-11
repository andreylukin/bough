package vtreal

// A model block that never ends (`while(true){}`) on the real goja
// runtime: the replay plugin answers the llm row from
// testdata/replay/codemodeinfinitelooptimeout.jsonl, but the codemode
// row is the real one, so the loop really spins. The step timeout must
// stop it with a visible message, esc must stop it within a second,
// and the same runtime must run the next block (it prints AFTERSPIN,
// spelled "AFTER"+"SPIN" in the code so only real output matches).

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// codemodeInfiniteLoopTimeoutConfig is replayConfig with the real
// codemode row in place of the tape's recorded results.
func codemodeInfiniteLoopTimeoutConfig(t *testing.T) string {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/codemodeinfinitelooptimeout.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	cfg := replayConfig(tape)
	out := strings.Replace(cfg,
		fmt.Sprintf("  plugin: replay\n  config: {file: %q, provide: codemode}\n", tape),
		"  plugin: codemode\n", 1)
	if out == cfg {
		t.Fatal("replayConfig's codemode row changed shape")
	}
	return out
}

// codemodeInfiniteLoopTimeoutSpin submits the spinning block and
// returns once it is on screen.
func codemodeInfiniteLoopTimeoutSpin(a *app) {
	a.t.Helper()
	a.typeText("spin forever")
	a.key(uv.KeyEnter, 0)
	a.waitFor("while(true)")
}

// codemodeInfiniteLoopTimeoutWait polls the screen for substr until
// limit passes.
func codemodeInfiniteLoopTimeoutWait(a *app, substr string, limit time.Duration) {
	a.t.Helper()
	at := time.Now()
	for !strings.Contains(a.text(), substr) {
		if time.Since(at) > limit {
			a.t.Fatalf("%q not on screen within %s:\n%s", substr, limit, a.text())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// codemodeInfiniteLoopTimeoutRecovers: after the spinning block ends,
// the same runtime runs the next block and turn n finishes.
func codemodeInfiniteLoopTimeoutRecovers(a *app, n int) {
	a.t.Helper()
	a.waitFor("AFTERSPIN")
	a.waitFor("RECOVERED")
	if !a.waitDone(n, 10*time.Second) {
		a.t.Fatalf("turn %d never finished after the loop was stopped:\n%s", n, a.text())
	}
	a.check("after recovery")
}

// Esc stops a spinning block within a second, and the runtime is
// reusable on the next turn.
func TestCodemodeInfiniteLoopTimeoutEsc(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, codemodeInfiniteLoopTimeoutConfig(t))
	codemodeInfiniteLoopTimeoutSpin(a)
	time.Sleep(300 * time.Millisecond) // the block is running, not just drawn

	a.key(uv.KeyEsc, 0)
	codemodeInfiniteLoopTimeoutWait(a, "cancelled", time.Second)
	if !a.waitDone(1, 5*time.Second) {
		t.Fatalf("cancelled turn never recorded:\n%s", a.text())
	}
	a.check("after esc")

	a.typeText("again")
	a.key(uv.KeyEnter, 0)
	codemodeInfiniteLoopTimeoutRecovers(a, 2)
}

// The step timeout ends the loop with a visible message; the model
// sees the error and its next block runs on the same runtime.
func TestCodemodeInfiniteLoopTimeoutFires(t *testing.T) {
	t.Run("small timeout env", func(t *testing.T) {
		t.Setenv("BOUGH_CODEMODE_TIMEOUT", "2s")
		a := startCfg(t, 100, 30, codemodeInfiniteLoopTimeoutConfig(t))
		codemodeInfiniteLoopTimeoutSpin(a)
		codemodeInfiniteLoopTimeoutWait(a, "timeout after 2s", 10*time.Second)
		codemodeInfiniteLoopTimeoutRecovers(a, 1)
	})
	t.Run("default 30s", func(t *testing.T) {
		if os.Getenv("BOUGH_SOAK_CODEMODE_INFINITE_LOOP_TIMEOUT") == "" {
			t.Skip("soak: waits out the 30s default timeout; set BOUGH_SOAK_CODEMODE_INFINITE_LOOP_TIMEOUT=1")
		}
		a := startCfg(t, 100, 30, codemodeInfiniteLoopTimeoutConfig(t))
		codemodeInfiniteLoopTimeoutSpin(a)
		codemodeInfiniteLoopTimeoutWait(a, "timeout after 30s", 45*time.Second)
		codemodeInfiniteLoopTimeoutRecovers(a, 1)
	})
}
