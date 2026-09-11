package vtreal

// Cost and context chips after a fork: two turns are played from a
// tape with fixed usage, /tree forks the session at turn 1 (the
// session tree's fork), and one more turn runs on the fork. The status
// bar must carry turn 1 plus the fork's turn only — not turn 2 (the
// original's lineage), not the llm's process-wide tally counted twice
// — and a quit and resume of the fork file must show the same chips.
//
// Tape usage (gpt-4o-mini, 128k window):
//
//	turn 1: 10k in, 1k out,   $0.10, last_in 12.8k (10% ctx)
//	turn 2: 40k in, 2k out,   $0.40, last_in 64k   (50% ctx)
//	fork:   20k in, 0.5k out, $0.20, last_in 32k   (25% ctx)

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// costContextChipAfterForkConfig is replayConfig plus the cost row,
// with the history row pinned to session when it is set.
func costContextChipAfterForkConfig(t *testing.T, tape, session string) string {
	t.Helper()
	yml := replayConfig(tape) + "- id: cost\n  plugin: cost\n"
	if session == "" {
		return yml
	}
	const row = "- id: history\n  plugin: history\n"
	if !strings.Contains(yml, row) {
		t.Fatalf("replayConfig no longer has a plain history row:\n%s", yml)
	}
	return strings.Replace(yml, row, row+"  config: {file: \""+session+"\"}\n", 1)
}

// costContextChipAfterForkHome is a fresh $HOME with a models.json
// stub, so the catalogue never refreshes from the network and the
// window comes from the built-in table.
func costContextChipAfterForkHome(t *testing.T) string {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(home, ".bough", "history"), 0o755); err != nil {
		t.Fatal(err)
	}
	stub := `{"v":2,"c":{"costtest":{"none":{}}}}`
	if err := os.WriteFile(filepath.Join(home, ".bough", "models.json"), []byte(stub), 0o644); err != nil {
		t.Fatal(err)
	}
	return home
}

// costContextChipAfterForkWant waits for the status bar to show every
// chip in want, then checks it shows none of not.
func costContextChipAfterForkWant(t *testing.T, a *app, where string, want []string, not ...string) {
	t.Helper()
	a.costBar(where, want...)
	bar := ""
	for _, l := range a.lines() {
		if strings.Contains(l, "? keys") {
			bar = l
		}
	}
	for _, n := range not {
		if strings.Contains(bar, n) {
			t.Errorf("%s: status bar shows %q (another lineage's or a doubled tally): %q\n%s", where, n, bar, a.text())
		}
	}
}

// costContextChipAfterForkFile is the fork file for seq, "" if none.
func costContextChipAfterForkFile(home, seq string) string {
	p, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*-f"+seq+".jsonl"))
	if len(p) != 1 {
		return ""
	}
	return p[0]
}

func TestCostContextChipAfterFork(t *testing.T) {
	t.Parallel()
	tape, err := filepath.Abs("testdata/replay/cost_context_chip_after_fork.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	home := costContextChipAfterForkHome(t)
	a := undoStart(t, home, 120, 30, costContextChipAfterForkConfig(t, tape, ""))

	// Wrong totals the fork could show: the original's ($0.50, 50%),
	// both lineages ($0.70), or the llm tally counted twice.
	wrong := []string{"$0.500", "$0.600", "$0.700", "$0.800", "50% ctx"}
	fork := []string{"$0.300", "25% ctx", "↑30.0k ↓1.5k"}

	t.Run("TwoTurns", func(t *testing.T) {
		followUpTurn(a, "turn one", 1)
		a.waitFor("CCF-ONE")
		costContextChipAfterForkWant(t, a, "after turn 1", []string{"$0.100", "10% ctx", "↑10.0k ↓1.0k"})
		followUpTurn(a, "turn two", 2)
		a.waitFor("CCF-TWO")
		costContextChipAfterForkWant(t, a, "after turn 2", []string{"$0.500", "50% ctx", "↑50.0k ↓3.0k"})
		a.check("two turns")
	})
	if t.Failed() {
		return
	}

	seq := ""
	for _, e := range followUpNewest(a) {
		if e.Kind == "input" {
			seq = strconv.FormatInt(e.Seq, 10)
			break
		}
	}

	t.Run("ForkAtTurnOne", func(t *testing.T) {
		a.typeText("/tree " + seq)
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return costContextChipAfterForkFile(home, seq) != "" }, "the fork file")
		costContextChipAfterForkWant(t, a, "on the fork, before a turn",
			[]string{"$0.100", "10% ctx", "↑10.0k ↓1.0k"}, wrong...)
		a.check("forked")
	})
	if t.Failed() {
		return
	}

	t.Run("TurnOnFork", func(t *testing.T) {
		a.typeText("fork turn")
		a.key(uv.KeyEnter, 0)
		followUpWaitDone(a, 2)
		a.waitFor("CCF-THREE")
		costContextChipAfterForkWant(t, a, "after the fork's turn", fork, wrong...)
		if in := followUpKinds(a, "input"); strings.Join(in, "|") != "turn one|fork turn" {
			t.Errorf("fork inputs = %q", in)
		}
		a.check("fork turn")
	})
	if t.Failed() {
		return
	}
	rewindAfterResumeAndForkQuit(t, a)

	path := costContextChipAfterForkFile(home, seq)
	if path == "" {
		t.Fatal("want one fork file")
	}
	if u := costUsages(t, path); len(u) != 2 {
		t.Errorf("fork file done usages = %v, want turn 1's and the fork turn's", u)
	}

	t.Run("ResumeForkSameChips", func(t *testing.T) {
		b := undoStart(t, home, 120, 30, costContextChipAfterForkConfig(t, tape, path))
		b.waitFor("CCF-THREE")
		// No "% ctx" here: the replay llm names no model until it has
		// answered a call, so the window is unknown before a turn (a
		// harness limit; a real provider's model comes from config).
		// Cost and tokens must match the live fork, and a stale
		// percentage must not show.
		costContextChipAfterForkWant(t, b, "fork resumed", []string{"$0.300", "↑30.0k ↓1.5k"}, wrong...)
		b.check("fork resumed")
	})
}
