package vtreal

// "/loop <interval> <prompt>" typed while a turn streams, and again
// right before quitting. bough has no /loop scheduler today (no
// command, no timer): the line must stay a command — never reach the
// model, never interleave with the streaming reply, never add a turn
// — and quitting with it "pending" must exit cleanly with no extra
// history. A restart on the same session must not fire anything.
// TestLoopScheduleDuringStreamAndQuit/registered asserts the command
// exists and is gated until it does.

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

// loopScheduleDuringStreamAndQuitConfig is resumeConfig with a slow
// stream (200ms per word, ~6s for the tape's 30 words).
func loopScheduleDuringStreamAndQuitConfig(tape, hist string) string {
	return strings.Replace(resumeConfig(tape, hist),
		fmt.Sprintf("config: {file: %q}\n- id: codemode", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 200}\n- id: codemode", tape), 1)
}

// loopScheduleDuringStreamAndQuitKinds counts entries by kind.
func loopScheduleDuringStreamAndQuitKinds(t *testing.T, path string) map[string]int {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	n := map[string]int{}
	for _, e := range entries {
		n[e.Kind]++
	}
	return n
}

func loopScheduleDuringStreamAndQuitExit(t *testing.T, a *app) {
	t.Helper()
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v", err)
		}
	case <-time.After(8 * time.Second):
		t.Fatalf("did not exit after two ctrl+c:\n%s", a.text())
	}
}

func TestLoopScheduleDuringStreamAndQuit(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/tab-title-slow.jsonl")

	t.Run("registered", func(t *testing.T) {
		t.Parallel()
		if os.Getenv("BOUGH_KNOWN_LOOP_SCHEDULE_DURING_STREAM_AND_QUIT") == "" {
			t.Skip("known gap: bough has no /loop command (plugins/commands registers none); set BOUGH_KNOWN_LOOP_SCHEDULE_DURING_STREAM_AND_QUIT to run")
		}
		a := startCfg(t, 100, 30, replayConfig(tape))
		before := len(commandsSystemEntries(a))
		a.typeText("/loop 1s ping")
		a.key(uv.KeyEnter, 0)
		time.Sleep(time.Second)
		if all := commandsSystemEntries(a); len(all) > before &&
			strings.Contains(strings.Join(all[before:], "\n"), "unknown command") {
			t.Fatalf("/loop is not a command:\n%s", a.text())
		}
	})

	t.Run("stream-quit-restart", func(t *testing.T) {
		t.Parallel()
		hist := filepath.Join(t.TempDir(), "session.jsonl")
		cfg := loopScheduleDuringStreamAndQuitConfig(tape, hist)
		a := startCfg(t, 100, 30, cfg)

		a.typeText("fix the flaky test")
		a.key(uv.KeyEnter, 0)
		a.waitFor("one two") // streaming has begun
		if resumeDones(hist) != 0 {
			t.Fatalf("turn finished before /loop could land mid-stream")
		}
		a.typeText("/loop 1s ping")
		a.key(uv.KeyEnter, 0)
		resumeWaitDones(t, a, hist, 1)
		time.Sleep(2500 * time.Millisecond) // > two 1s "intervals"

		k := loopScheduleDuringStreamAndQuitKinds(t, hist)
		if k["input"] != 1 || k["done"] != 1 || k["assistant"] != 1 {
			t.Fatalf("/loop mid-stream changed the turn log: %v\n%s", k, a.text())
		}
		entries, _ := history.Read(hist)
		for _, e := range entries {
			if s, _ := e.Data["text"].(string); e.Kind == "assistant" && strings.Contains(s, "ping") {
				t.Fatalf("/loop text interleaved into the reply: %q", s)
			}
		}
		if s := a.settled(); !strings.Contains(s, "thirty") {
			t.Fatalf("reply did not finish on screen:\n%s", s)
		}
		a.check("after /loop mid-stream")

		// Again right before quitting: exit clean, nothing new logged.
		a.typeText("/loop 1s ping")
		a.key(uv.KeyEnter, 0)
		loopScheduleDuringStreamAndQuitExit(t, a)
		after := loopScheduleDuringStreamAndQuitKinds(t, hist)
		if after["input"] != 1 || after["done"] != 1 || after["assistant"] != 1 {
			t.Fatalf("quit with /loop pending added turns: %v", after)
		}

		// Restart on the same session: nothing fires.
		b := startCfg(t, 100, 30, cfg)
		b.waitFor("thirty")
		time.Sleep(2500 * time.Millisecond)
		if again := loopScheduleDuringStreamAndQuitKinds(t, hist); again["input"] != 1 || again["done"] != 1 || again["assistant"] != 1 {
			t.Fatalf("restart fired a turn: %v\n%s", again, b.text())
		}
		b.check("after restart")
		loopScheduleDuringStreamAndQuitExit(t, b)
	})
}
