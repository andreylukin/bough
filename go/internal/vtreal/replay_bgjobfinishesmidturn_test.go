package vtreal

// A background job that finishes while a later turn is still
// streaming. Turn 1 starts a detached job gated on a fifo; turn 2's
// reply streams slowly and the test opens the fifo only once that
// stream is on screen, so the job lands mid-turn every run. Turn 2 is
// text only (no block), so nothing lands the notice inside the turn:
// it must wait for the turn's done, then open exactly one wake turn —
// never be spliced into turn 2 as a user input, never twice.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

// bgjobfinishesmidturnTape writes the three-turn tape for a job gated
// on fifo and returns its path.
func bgjobfinishesmidturnTape(t *testing.T, fifo, story string) string {
	t.Helper()
	cmd := fmt.Sprintf("cat %s >/dev/null; echo GATED-DONE", fifo)
	code := fmt.Sprintf("console.log(tools.bash(%q, 120))\n", cmd)
	wake := "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\njob 1 [exited 0] " + cmd + " (1s)\nGATED-DONE"
	rows := []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "start the gated job"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"code", map[string]any{"text": code}},
		{"result", map[string]any{"code": code, "text": "job 1 started in the background (limit 2m0s): " + cmd + "\n"}},
		{"assistant", map[string]any{"text": "```stop\nStarted job 1.\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": "tell me a long story"}},
		{"assistant", map[string]any{"text": "```stop\n" + story + "\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": wake}},
		{"assistant", map[string]any{"text": "```stop\nJob 1 landed with GATED-DONE.\n```"}},
		{"done", map[string]any{"text": ""}},
	}
	var b strings.Builder
	for i, r := range rows {
		line, err := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": r.kind, "data": r.data})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	p := filepath.Join(t.TempDir(), "bgjob-midturn.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// bgjobfinishesmidturnConfig is jobsConfig with a slow llm stream.
func bgjobfinishesmidturnConfig(tape string, delayMS int) string {
	return strings.Replace(jobsConfig(tape),
		fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: %d}", tape, delayMS), 1)
}

func TestBgjobFinishesMidTurn(t *testing.T) {
	t.Parallel()
	dir, err := os.MkdirTemp("", "bgjob") // short: the path rides in the command
	if err != nil {
		t.Fatal(err)
	}
	fifo := filepath.Join(dir, "gate")
	t.Cleanup(func() { os.Remove(fifo); os.Remove(dir) })
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	words := make([]string, 80)
	for i := range words {
		words[i] = fmt.Sprintf("w%02d", i)
	}
	words[0] = "STORY-BEGINS"
	words[len(words)-1] = "STORY-ENDS"
	tape := bgjobfinishesmidturnTape(t, fifo, strings.Join(words, " "))
	a := startCfg(t, 100, 30, bgjobfinishesmidturnConfig(tape, 40))
	a.check("boot")

	jobsSay(a, "start the gated job")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 1") }, "the job strip to name job 1")

	jobsSay(a, "tell me a long story")
	a.waitUntil(func(s string) bool { return strings.Contains(s, "STORY-BEGINS") }, "turn 2 to start streaming")
	if a.doneCount() != 1 {
		t.Fatalf("turn 2 finished before the gate opened; the stream is too fast:\n%s", a.text())
	}
	// Open the gate: cat is the reader, so this returns at once.
	f, err := os.OpenFile(fifo, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	f.Close()
	if a.doneCount() != 1 {
		t.Fatalf("gate opened after turn 2 finished: not a mid-turn finish")
	}

	if !a.waitDone(3, 60*time.Second) {
		t.Fatalf("the finished job never woke a turn after turn 2:\n%s", a.text())
	}
	a.waitFor("Job 1 landed with GATED-DONE")

	t.Run("notice not spliced into turn 2", func(t *testing.T) {
		es := jobsHistory(a)
		in2, done2 := -1, -1
		for i, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "input" && text == "tell me a long story" {
				in2 = i
			}
			if in2 >= 0 && done2 < 0 && e.Kind == "done" {
				done2 = i
			}
		}
		if in2 < 0 || done2 < 0 {
			t.Fatalf("turn 2's input/done missing from history (%d, %d)", in2, done2)
		}
		for _, e := range es[in2+1 : done2] {
			text, _ := e.Data["text"].(string)
			if e.Kind == "input" || e.Kind == "job" || strings.Contains(text, "[background job]") {
				t.Errorf("job notice landed inside turn 2: %s %q", e.Kind, text)
			}
		}
		wakes := 0
		for i, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
				wakes++
				if i < done2 {
					t.Errorf("wake input at entry %d precedes turn 2's done at %d", i, done2)
				}
				if !strings.Contains(text, "GATED-DONE") {
					t.Errorf("wake input does not carry the job output: %q", text)
				}
			}
		}
		if wakes != 1 {
			t.Errorf("want exactly one wake input, got %d", wakes)
		}
	})

	t.Run("turn 2 reply intact", func(t *testing.T) {
		for _, e := range jobsHistory(a) {
			text, _ := e.Data["text"].(string)
			if e.Kind == "assistant" && strings.Contains(text, "STORY-BEGINS") && !strings.Contains(text, "STORY-ENDS") {
				t.Errorf("turn 2's reply was cut short: %q", text)
			}
		}
	})

	t.Run("no duplicate wake turn", func(t *testing.T) {
		time.Sleep(2 * time.Second)
		if n := a.doneCount(); n != 3 {
			t.Errorf("want 3 finished turns, got %d:\n%s", n, a.text())
		}
	})

	t.Run("job strip clears", func(t *testing.T) {
		a.waitUntil(func(s string) bool {
			ls := strings.Split(s, "\n")
			c := composerRow(ls)
			if c < 0 {
				return false
			}
			for _, l := range ls[c+1:] {
				if strings.Contains(l, "job 1") || strings.Contains(l, "GATED-DONE") {
					return false
				}
			}
			return true
		}, "the job strip to clear")
		a.check("after wake")
		if s := a.settled(); strings.Contains(s, "[background job]") {
			t.Errorf("wake preamble leaked onto the screen:\n%s", s)
		}
	})
}
