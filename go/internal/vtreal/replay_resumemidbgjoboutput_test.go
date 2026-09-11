package vtreal

// Resume mid background-job output: turn 1 starts a detached job that
// reads lines from a fifo the test holds open; the test writes a known
// number of lines, SIGKILLs bough, then restarts it with --resume. The
// job belonged to the dead process, so the resumed session must not
// pretend it is still running (no strip row, no spinner), must say it
// ended with the process, and must never open a wake turn for it —
// not even when the orphaned command finishes after the restart.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// resumemidbgjoboutputTape writes a tape whose first turn (when first)
// starts `cat fifo` as a background job, followed by the extra turns.
func resumemidbgjoboutputTape(t *testing.T, dir, name, fifo string, first bool, extra ...string) string {
	t.Helper()
	cmd := "cat " + fifo
	code := fmt.Sprintf("console.log(tools.bash(%q, 120))\n", cmd)
	type row struct {
		kind string
		data map[string]any
	}
	rows := []row{{"meta", map[string]any{"cwd": "/tmp/demo"}}}
	if first {
		rows = append(rows,
			row{"input", map[string]any{"text": "start the fifo job"}},
			row{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
			row{"code", map[string]any{"text": code}},
			row{"result", map[string]any{"code": code, "text": "job 1 started in the background (limit 2m0s): " + cmd + "\n"}},
			row{"assistant", map[string]any{"text": "```stop\nStarted job 1.\n```"}},
			row{"done", map[string]any{"text": ""}})
	}
	for _, in := range extra {
		rows = append(rows,
			row{"input", map[string]any{"text": in}},
			row{"assistant", map[string]any{"text": "```stop\nReply to " + in + ".\n```"}},
			row{"done", map[string]any{"text": ""}})
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
	p := filepath.Join(dir, name)
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// resumemidbgjoboutputStripClear reports whether no row under the
// composer names job 1.
func resumemidbgjoboutputStripClear(s string) bool {
	ls := strings.Split(s, "\n")
	c := composerRow(ls)
	if c < 0 {
		return false
	}
	for _, l := range ls[c+1:] {
		if strings.Contains(l, "job 1") {
			return false
		}
	}
	return true
}

func TestResumeMidBgjobOutput(t *testing.T) {
	t.Parallel()
	dir, err := os.MkdirTemp("", "rmbj") // short: the path rides in the command
	if err != nil {
		t.Fatal(err)
	}
	fifo := filepath.Join(dir, "feed")
	t.Cleanup(func() { os.Remove(fifo); os.Remove(dir) })
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}

	home := t.TempDir()
	tape := resumemidbgjoboutputTape(t, home, "tape1.jsonl", fifo, true)
	a := crashResumeIntegrityStart(t, home, jobsConfig(tape))
	jobsSay(a, "start the fifo job")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 1") && strings.Contains(s, "cat ") },
		"the job strip to name job 1")

	// Opening for write blocks until cat holds the read end: the job is
	// live and reading. Two lines go through; the writer stays open so
	// cat is still mid-output when bough dies.
	w, err := os.OpenFile(fifo, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { w.Close() }) // EOF lets the orphaned cat exit
	for i := 1; i <= 2; i++ {
		if _, err := fmt.Fprintf(w, "FEED-%d\n", i); err != nil {
			t.Fatal(err)
		}
	}
	time.Sleep(300 * time.Millisecond) // let cat drain both lines
	if err := a.cmd.Process.Signal(syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	_ = a.term.Wait(a.cmd)

	session := crashResumeIntegritySession(t, home)
	id := strings.TrimSuffix(filepath.Base(session), ".jsonl")
	next := resumemidbgjoboutputTape(t, home, "tape2.jsonl", fifo, false, "and now")
	b := crashResumeIntegrityStart(t, home, jobsConfig(next), "--resume", id)
	b.waitFor("resumed ")
	b.check("resumed boot")
	s := b.settled()

	t.Run("job_strip_empty", func(t *testing.T) {
		if !resumemidbgjoboutputStripClear(s) {
			t.Errorf("resumed session shows the dead job as running:\n%s", s)
		}
	})

	t.Run("job_marked_ended", func(t *testing.T) {
		low := strings.ToLower(s)
		ok := false
		for _, w := range []string{"orphan", "lost", "ended", "killed", "interrupted", "no longer running"} {
			if strings.Contains(low, w) {
				ok = true
			}
		}
		if !ok {
			t.Errorf("resumed transcript never says job 1 ended with the old process:\n%s", s)
		}
	})

	t.Run("no_wake_for_dead_job", func(t *testing.T) {
		// The orphaned cat finishes now, after the restart.
		fmt.Fprintf(w, "FEED-3\n")
		w.Close()
		time.Sleep(2 * time.Second)
		if n := resumeDones(session); n != 1 {
			t.Errorf("want 1 finished turn (the pre-crash one), got %d:\n%s", n, b.text())
		}
		for _, e := range crashResumeIntegrityLines(t, session) {
			text, _ := e.Data["text"].(string)
			if e.Kind == "job" || (e.Kind == "input" && strings.HasPrefix(text, "[background job]")) {
				t.Errorf("dead job produced a %s entry after resume: %q", e.Kind, text)
			}
		}
		if st := b.settled(); strings.Contains(st, "FEED-3") || strings.Contains(st, "▸ job") {
			t.Errorf("dead job's output reached the resumed screen:\n%s", st)
		}
	})

	t.Run("new_turn_runs", func(t *testing.T) {
		b.typeText("and now")
		b.key(uv.KeyEnter, 0)
		b.waitFor("Reply to and now.")
		if !resumemidbgjoboutputStripClear(b.settled()) {
			t.Errorf("job strip reappeared after a new turn:\n%s", b.text())
		}
		b.check("after the new turn")
	})
}
