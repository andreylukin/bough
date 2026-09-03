package loop_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"

	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/tools"
)

// Enter while a turn runs steers it through the real model and loop:
// the steer lands after the block in flight, the reply's second block
// never runs, and the model's next reply follows the steer row.
func TestEnterSteersRunningTurnEndToEnd(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	gate := filepath.Join(dir, "gate")
	two := filepath.Join(dir, "two")
	stub := &uitest.Script{Replies: []string{
		"one, then two:\n" +
			uitest.Bash("while [ ! -e "+gate+" ]; do sleep 0.05; done; echo ONE_RAN") + "\n" +
			uitest.Bash("touch "+two+"; echo TWO_RAN"),
		"steered ok",
	}}
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) },
		"codemode", "tools-basic", "loop")
	d.Say("go")
	d.WaitFor("Ran: while") // block one is executing: the turn is live
	d.Say("use B instead")  // enter mid-turn is a steer, not a queued prompt
	d.WaitFor("(steer · pending)")
	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	d.WaitFor("steered ok")
	f := d.Frame()
	if _, err := os.Stat(two); err == nil || strings.Contains(f, "TWO_RAN") {
		t.Fatalf("block two ran after the steer:\n%s", f)
	}
	if !strings.Contains(f, "❯ use B instead (steer)") || strings.Contains(f, "pending") {
		t.Fatalf("landed steer row missing:\n%s", f)
	}
	if strings.Contains(f, "(queued)") {
		t.Fatalf("a steer is not a queued follow-up:\n%s", f)
	}
	iS, iR := strings.Index(f, "❯ use B instead"), strings.Index(f, "steered ok")
	if iS < 0 || iR < iS {
		t.Fatalf("the reply should follow the steer row (steer@%d reply@%d):\n%s", iS, iR, f)
	}
	if stub.Calls != 2 {
		t.Fatalf("model calls = %d, want 2 (the steer asks once more)", stub.Calls)
	}
	d.WaitUntil(func(f string) bool { return !strings.ContainsAny(f, "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏") }, "spinner to stop")
}

// alt+enter while a turn runs keeps today's behavior: the line waits
// for the turn to end and starts its own.
func TestAltEnterQueuesFollowUpEndToEnd(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	gate := filepath.Join(dir, "gate")
	stub := &uitest.Script{Replies: []string{
		uitest.Bash("while [ ! -e " + gate + " ]; do sleep 0.05; done; echo ONE_RAN"),
		"first turn over",
		"second turn over",
	}}
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) },
		"codemode", "tools-basic", "loop")
	d.Say("go")
	d.WaitFor("Ran: while")
	d.Type("and then this")
	d.Press("alt+enter")
	d.WaitFor("❯ and then this (queued)")
	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	d.WaitFor("second turn over")
	f := d.Frame()
	iA, iB := strings.Index(f, "first turn over"), strings.Index(f, "second turn over")
	if !(iA >= 0 && iA < iB) {
		t.Fatalf("follow-up should run as its own turn after the first (first@%d second@%d):\n%s", iA, iB, f)
	}
	if !strings.Contains(f, "❯ and then this") || strings.Contains(f, "(queued)") {
		t.Fatalf("the follow-up row should be there, no longer queued:\n%s", f)
	}
	if stub.Calls != 3 {
		t.Fatalf("model calls = %d, want 3 (two blocks-free turns after the first reply)", stub.Calls)
	}
	if strings.Contains(f, "steer") {
		t.Fatalf("a follow-up is not a steer:\n%s", f)
	}
}
