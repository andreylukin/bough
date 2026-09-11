package vtreal

// Nudge, system and title entries. The replay plugin only feeds
// assistant and result entries, so tape-borne nudge/system/title
// entries reach the screen by RESUMING the tape (history row
// config.file, what -r sets); the loop's own nudge is exercised live
// with a reply that ends in a question.

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

// nudgeSystemKinds counts entry kinds across the given history files.
func nudgeSystemKinds(paths ...string) map[string]int {
	n := map[string]int{}
	for _, p := range paths {
		entries, err := history.Read(p)
		if err != nil {
			continue
		}
		for _, e := range entries {
			n[e.Kind]++
		}
	}
	return n
}

// nudgeSystemResume boots on a copy of the fixture tape as the
// resumed session; returns the app and the copy's path.
func nudgeSystemResume(t *testing.T) (*app, string) {
	t.Helper()
	src, _ := filepath.Abs("testdata/replay/nudge-system.jsonl")
	b, err := os.ReadFile(src)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "nudge-system.jsonl")
	if err := os.WriteFile(path, b, 0o644); err != nil {
		t.Fatal(err)
	}
	yml := strings.Replace(replayConfig(src),
		"- id: history\n  plugin: history\n",
		fmt.Sprintf("- id: history\n  plugin: history\n  config: {file: %q}\n", path), 1)
	a := startCfg(t, 120, 50, yml)
	a.waitFor("Second answer.")
	return a, path
}

func TestNudgeSystemResumeRows(t *testing.T) {
	t.Parallel()
	a, _ := nudgeSystemResume(t)
	a.check("resumed")
	s := a.settled()
	var sysHead, nudge, one bool
	for _, l := range strings.Split(s, "\n") {
		l = strings.TrimSpace(l)
		switch {
		case strings.HasPrefix(l, "▸ system (3 lines)") && strings.Contains(l, "SYSHEAD"):
			sysHead = true
		case strings.HasPrefix(l, "nudge ") && strings.Contains(l, "NUDGEHEAD"):
			nudge = true
		case strings.Contains(l, "SYSONE single line note"):
			one = true
		}
	}
	if !sysHead {
		t.Errorf("multi-line system entry: want one collapsed \"▸ system (3 lines) SYSHEAD…\" row:\n%s", s)
	}
	if strings.Contains(s, "SYSBODY") {
		t.Errorf("collapsed system entry leaked its body:\n%s", s)
	}
	if !nudge {
		t.Errorf("nudge entry: want a row marked \"nudge NUDGEHEAD…\":\n%s", s)
	}
	if !one {
		t.Errorf("single-line system entry missing:\n%s", s)
	}
}

func TestNudgeSystemResumeTitle(t *testing.T) {
	t.Parallel()
	a, _ := nudgeSystemResume(t)
	s := a.settled()
	bar := ""
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "? keys") {
			bar = l
		}
	}
	if !strings.HasPrefix(strings.TrimSpace(bar), "Nudge fixture session") {
		t.Errorf("status bar left side = %q, want the title entry:\n%s", bar, s)
	}
	if c := strings.Count(s, "Nudge fixture session"); c != 1 {
		t.Errorf("title shown %d times, want only in the status bar:\n%s", c, s)
	}
}

func TestNudgeSystemResumeDoneCount(t *testing.T) {
	t.Parallel()
	a, path := nudgeSystemResume(t)
	k := nudgeSystemKinds(path)
	if k["done"] != 2 || k["nudge"] != 1 || k["system"] != 2 {
		t.Fatalf("resumed file kinds = %v, want done 2, nudge 1, system 2:\n%s", k, a.text())
	}
	a.typeText("third question")
	a.key(uv.KeyEnter, 0)
	deadline := time.Now().Add(30 * time.Second)
	for nudgeSystemKinds(path)["done"] < 3 && time.Now().Before(deadline) {
		time.Sleep(50 * time.Millisecond)
	}
	a.check("after live turn")
	time.Sleep(500 * time.Millisecond)
	if k := nudgeSystemKinds(path); k["done"] != 3 {
		t.Errorf("done entries = %d after one more turn, want 3 (nudge/system are not turns):\n%s", k["done"], a.text())
	}
}

func TestNudgeSystemLiveNudge(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/nudge-system-live.jsonl")
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.typeText("go")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("All done.")
	a.check("after nudge")
	s := a.settled()
	if !strings.Contains(s, "asking again (1/2)") {
		t.Errorf("want the loop's one-line nudge note:\n%s", s)
	}
	if strings.Contains(s, "Shall I go on?") {
		t.Errorf("superseded reply still on screen:\n%s", s)
	}
	time.Sleep(500 * time.Millisecond)
	if n := a.doneCount(); n != 1 {
		t.Errorf("doneCount = %d, want 1 (the nudge is not a turn):\n%s", n, a.text())
	}
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	if k := nudgeSystemKinds(paths...); k["nudge"] != 1 {
		t.Errorf("nudge entries = %d, want 1:\n%s", k["nudge"], a.text())
	}
}
