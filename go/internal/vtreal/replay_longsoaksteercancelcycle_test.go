package vtreal

// long-soak-steer-cancel-cycle: N iterations of submit, wait for the
// first streamed word, steer, esc, submit again. Every iteration must
// end idle, history must hold exactly one cancel and two dones per
// iteration, the thread count (bough has no debug endpoint; threads
// stand in for goroutines, as in replay_soakidleandchurn_test.go) and
// RSS must plateau, and only one status bar may be on screen.
//
// The tape repeats one cycle per iteration: a long slow reply that
// the esc cancels, then a short reply for the resubmit. 8 iterations
// by default; the 50-iteration soak is env-gated:
//
//	BOUGH_SOAK_LONG_SOAK_STEER_CANCEL_CYCLE=1 go test ./internal/vtreal -run TestLongSoakSteerCancelCycle -timeout 30m

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func longSoakSteerCancelCycleTape(t *testing.T, n int) string {
	t.Helper()
	var entries []map[string]any
	for i := 1; i <= n; i++ {
		long := fmt.Sprintf("LONGSTART%d", i) + strings.Repeat(" filler", 200) + fmt.Sprintf(" LONGEND%d", i)
		entries = append(entries,
			map[string]any{"kind": "input", "data": map[string]any{"text": fmt.Sprintf("cycle %d", i)}},
			map[string]any{"kind": "assistant", "data": map[string]any{"text": long}},
			map[string]any{"kind": "done", "data": map[string]any{"text": ""}},
			map[string]any{"kind": "input", "data": map[string]any{"text": fmt.Sprintf("again %d", i)}},
			map[string]any{"kind": "assistant", "data": map[string]any{"text": fmt.Sprintf("```stop\nSHORTREPLY%d\n```", i)}},
			map[string]any{"kind": "done", "data": map[string]any{"text": ""}},
		)
	}
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = "2026-09-11T10:00:00Z"
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "cycle.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// longSoakSteerCancelCycleKinds counts cancelled and done entries.
func longSoakSteerCancelCycleKinds(a *app) (cancelled, done int) {
	for _, e := range a.steerEntries() {
		switch e.Kind {
		case "cancelled":
			cancelled++
		case "done":
			done++
		}
	}
	return
}

func longSoakSteerCancelCycleRun(t *testing.T, n int) {
	a := startCfg(t, 100, 30, cancelConfig(longSoakSteerCancelCycleTape(t, n), 60))
	pid := a.cmd.Process.Pid
	a.check("boot")
	warm := max(n/5, 2)
	var baseRSS, baseThreads, peakRSS, peakThreads int
	for i := 1; i <= n; i++ {
		where := fmt.Sprintf("iteration %d/%d", i, n)
		a.typeText(fmt.Sprintf("cycle %d\r", i))
		a.waitFor(fmt.Sprintf("LONGSTART%d", i))
		a.typeText(fmt.Sprintf("steer %d\r", i))
		a.waitFor(fmt.Sprintf("steer %d (steer · pending)", i))
		a.typeText("\x1b")
		a.waitUntil(func(string) bool { c, _ := longSoakSteerCancelCycleKinds(a); return c >= i },
			where+": the cancel recorded")
		a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) }, where+": spinner gone after esc")
		// A pending steer may come back into the composer; clear it.
		ls := strings.Split(a.settled(), "\n")
		if r := composerRow(ls); r >= 0 && !cancelComposerEmpty(ls[r]) {
			a.typeText("\x15")
			a.waitUntil(func(s string) bool {
				l := strings.Split(s, "\n")
				r := composerRow(l)
				return r >= 0 && cancelComposerEmpty(l[r])
			}, where+": composer to clear")
		}
		a.typeText(fmt.Sprintf("again %d\r", i))
		if !a.waitDone(3*i, 60*time.Second) {
			t.Fatalf("%s: resubmitted turn never finished:\n%s", where, a.text())
		}
		a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) }, where+": idle")
		s := a.settled()
		a.check(where)
		if !strings.Contains(s, fmt.Sprintf("SHORTREPLY%d", i)) {
			t.Errorf("%s: resubmit did not render its own reply:\n%s", where, s)
		}
		for _, bad := range []string{fmt.Sprintf("LONGEND%d", i), "end of tape"} {
			if strings.Contains(s, bad) {
				t.Errorf("%s: %q on screen:\n%s", where, bad, s)
			}
		}
		if c := strings.Count(s, "? keys"); c != 1 {
			t.Errorf("%s: %d status bars on screen:\n%s", where, c, s)
		}
		if c, d := longSoakSteerCancelCycleKinds(a); c != i || d != 2*i {
			t.Errorf("%s: history has %d cancelled, %d done; want %d, %d", where, c, d, i, 2*i)
		}
		if t.Failed() {
			return
		}
		rss, th := soakidleandchurnSample(t, pid)
		if i == warm {
			baseRSS, baseThreads = rss, th
		}
		if i > warm {
			peakRSS, peakThreads = max(peakRSS, rss), max(peakThreads, th)
		}
		if i == 1 || i == warm || i == n || i%10 == 0 {
			t.Logf("iteration %d: rss %d KiB, threads %d", i, rss, th)
		}
	}
	if n > warm {
		if limit := baseRSS*2 + 64*1024; peakRSS > limit {
			t.Errorf("RSS did not plateau: %d KiB at %d, peak %d KiB (limit %d)", baseRSS, warm, peakRSS, limit)
		}
		if limit := baseThreads + 8; peakThreads > limit {
			t.Errorf("threads did not plateau: %d at %d, peak %d (limit %d)", baseThreads, warm, peakThreads, limit)
		}
	}
}

func TestLongSoakSteerCancelCycleShort(t *testing.T) {
	t.Parallel()
	longSoakSteerCancelCycleRun(t, 8)
}

func TestLongSoakSteerCancelCycleFull(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_LONG_SOAK_STEER_CANCEL_CYCLE") == "" {
		t.Skip("set BOUGH_SOAK_LONG_SOAK_STEER_CANCEL_CYCLE=1 to run the 50-iteration soak")
	}
	longSoakSteerCancelCycleRun(t, 50)
}
