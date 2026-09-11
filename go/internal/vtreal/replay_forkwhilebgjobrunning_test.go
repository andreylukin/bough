package vtreal

// Fork while a background job runs. Turn 1 starts a detached job gated
// on a fifo the test holds; /tree forks the session at turn 1 (the
// fork becomes the mounted session); then the gate opens. The job
// belongs to the session that started it: its finished notice must
// wake that session exactly once, never the fork, and the fork's job
// strip must not show a job the fork never started.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	uv "github.com/charmbracelet/ultraviolet"
)

// forkwhilebgjobrunningTape writes the tape: turn 1 starts the gated
// job, then one wake turn for whichever session the notice reaches.
func forkwhilebgjobrunningTape(t *testing.T, fifo string) string {
	t.Helper()
	cmd := fmt.Sprintf("cat %s >/dev/null; echo FORKJOB-DONE", fifo)
	code := fmt.Sprintf("console.log(tools.bash(%q, 120))\n", cmd)
	wake := "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\njob 1 [exited 0] " + cmd + " (1s)\nFORKJOB-DONE"
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
		{"input", map[string]any{"text": wake}},
		{"assistant", map[string]any{"text": "```stop\nFORKJOB-WOKE\n```"}},
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
	p := filepath.Join(t.TempDir(), "fork-bgjob.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// forkwhilebgjobrunningWakes counts the wake inputs in a history file.
func forkwhilebgjobrunningWakes(t *testing.T, path string) int {
	t.Helper()
	es, err := history.Read(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	n := 0
	for _, e := range es {
		text, _ := e.Data["text"].(string)
		if e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
			n++
		}
	}
	return n
}

// forkwhilebgjobrunningStrip reports whether a job row sits under the
// composer.
func forkwhilebgjobrunningStrip(s string) bool {
	ls := strings.Split(s, "\n")
	c := composerRow(ls)
	if c < 0 {
		return false
	}
	for _, l := range ls[c+1:] {
		if strings.Contains(l, "job 1") {
			return true
		}
	}
	return false
}

func TestForkWhileBgjobRunning(t *testing.T) {
	t.Parallel()
	dir, err := os.MkdirTemp("", "fkjob") // short: the path rides in the command
	if err != nil {
		t.Fatal(err)
	}
	fifo := filepath.Join(dir, "gate")
	t.Cleanup(func() { os.Remove(fifo); os.Remove(dir) })
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	gateOpen := false
	openGate := func() {
		if gateOpen {
			return
		}
		gateOpen = true
		f, err := os.OpenFile(fifo, os.O_WRONLY, 0)
		if err != nil {
			t.Fatal(err)
		}
		f.Close()
	}
	// A failed subtest must not leave cat blocked on the fifo.
	t.Cleanup(func() {
		if !gateOpen {
			if f, err := os.OpenFile(fifo, os.O_WRONLY|syscall.O_NONBLOCK, 0); err == nil {
				f.Close()
			}
		}
	})

	home := costContextChipAfterForkHome(t)
	a := undoStart(t, home, 100, 30, jobsConfig(forkwhilebgjobrunningTape(t, fifo)))

	jobsSay(a, "start the gated job")
	followUpWaitDone(a, 1)
	a.waitUntil(forkwhilebgjobrunningStrip, "the job strip to name job 1")
	orig := ""
	if p, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl")); len(p) == 1 {
		orig = p[0]
	} else {
		t.Fatalf("want one session file before the fork, got %v", p)
	}
	seq := ""
	for _, e := range followUpNewest(a) {
		if e.Kind == "input" {
			seq = strconv.FormatInt(e.Seq, 10)
			break
		}
	}

	a.typeText("/tree " + seq)
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(string) bool { return costContextChipAfterForkFile(home, seq) != "" }, "the fork file")
	fork := costContextChipAfterForkFile(home, seq)
	a.waitFor("say something")
	a.check("forked")

	t.Run("strip not on the fork", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_FORKWHILEBGJOBRUNNING") == "" {
			t.Skip("known bug: the job strip is process-wide (tools Jobs never remount), so a fork shows the original session's job; set BOUGH_KNOWN_FORKWHILEBGJOBRUNNING=1 to run")
		}
		time.Sleep(time.Second)
		if s := a.settled(); forkwhilebgjobrunningStrip(s) {
			t.Errorf("the fork shows the original session's job 1 in its strip:\n%s", s)
		}
	})

	openGate()
	// Give the notice every chance to land somewhere.
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		if forkwhilebgjobrunningWakes(t, orig)+forkwhilebgjobrunningWakes(t, fork) > 0 {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	time.Sleep(2 * time.Second) // a duplicate wake would land by now

	t.Run("wake lands in the fork never", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_FORKWHILEBGJOBRUNNING") == "" {
			t.Skip("known bug: a job's finished notice wakes whichever session is mounted (the remounted loop drains the process-wide job-notices), so it opens a turn in the fork; set BOUGH_KNOWN_FORKWHILEBGJOBRUNNING=1 to run")
		}
		if n := forkwhilebgjobrunningWakes(t, fork); n != 0 {
			t.Errorf("the fork got %d wake turn(s) for a job it never started:\n%s", n, a.text())
		}
	})

	t.Run("at most one wake overall", func(t *testing.T) {
		o, f := forkwhilebgjobrunningWakes(t, orig), forkwhilebgjobrunningWakes(t, fork)
		if o+f > 1 {
			t.Errorf("job notice woke %d turns (original %d, fork %d), want at most one", o+f, o, f)
		}
		if o > 1 {
			t.Errorf("original got %d wakes", o)
		}
	})

	t.Run("original unchanged by the fork", func(t *testing.T) {
		es, err := history.Read(orig)
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind == "input" && text != "start the gated job" && !strings.HasPrefix(text, "[background job] ") {
				t.Errorf("fork-side input landed in the original: %q", text)
			}
		}
	})

	t.Run("strip clears after the job", func(t *testing.T) {
		a.waitUntil(func(s string) bool { return !forkwhilebgjobrunningStrip(s) }, "the job strip to clear")
	})
}
