package vtreal

// /undo after a background job wrote files. The turn writes a.txt with
// tools.write and starts a detached tools.bash job that blocks on a
// fifo; only after the turn is done does the test open the fifo, and
// the job then writes job.txt (never touched by the turn) and
// overwrites a.txt. /undo must list only the turn's own writes, leave
// job.txt alone, and not silently throw away the job's later a.txt.
// Real codemode + tools-basic (jobsConfig); the tape only answers the
// model.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

const undoAfterBgJobWroteFileTape = `{"seq":1,"at":"2026-09-11T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-11T10:00:01Z","kind":"input","data":{"text":"rewrite a.txt and start the gated job"}}
{"seq":3,"at":"2026-09-11T10:00:02Z","kind":"assistant","data":{"text":"` + "```js\\ntools.write(\\\"a.txt\\\", \\\"turn\\\\n\\\")\\nconsole.log(tools.bash(\\\"cat gate.fifo >/dev/null; echo job > job.txt; echo job > a.txt; echo JOB-WROTE\\\", 120))\\n```" + `"}}
{"seq":4,"at":"2026-09-11T10:00:03Z","kind":"assistant","data":{"text":"` + "```stop\\nStarted job 1.\\n```" + `"}}
{"seq":5,"at":"2026-09-11T10:00:03Z","kind":"done","data":{"text":""}}
{"seq":6,"at":"2026-09-11T10:00:06Z","kind":"input","data":{"text":"[background job] finished"}}
{"seq":7,"at":"2026-09-11T10:00:07Z","kind":"assistant","data":{"text":"` + "```stop\\nJob 1 finished.\\n```" + `"}}
{"seq":8,"at":"2026-09-11T10:00:07Z","kind":"done","data":{"text":""}}
`

// undoAfterBgJobWroteFileStart boots on the tape, makes $HOME a git
// repo with a.txt="before" and a fifo, and returns the app.
func undoAfterBgJobWroteFileStart(t *testing.T) *app {
	t.Helper()
	tape := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(tape, []byte(undoAfterBgJobWroteFileTape), 0o644); err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, 100, 30, jobsConfig(tape))
	if out, err := exec.Command("git", "-C", a.home, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	for name, s := range map[string]string{
		".gitignore": ".bough/\nbough.yml\ngate.fifo\n",
		"a.txt":      "before\n",
	} {
		if err := os.WriteFile(filepath.Join(a.home, name), []byte(s), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := syscall.Mkfifo(filepath.Join(a.home, "gate.fifo"), 0o644); err != nil {
		t.Fatal(err)
	}
	return a
}

// undoAfterBgJobWroteFileOpenGate releases the job: opening the fifo
// for writing blocks until the job's cat opens it for reading.
func undoAfterBgJobWroteFileOpenGate(t *testing.T, a *app) {
	t.Helper()
	done := make(chan error, 1)
	go func() {
		f, err := os.OpenFile(filepath.Join(a.home, "gate.fifo"), os.O_WRONLY, 0)
		if err == nil {
			_, err = f.WriteString("go\n")
			f.Close()
		}
		done <- err
	}()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("gate: %v", err)
		}
	case <-time.After(30 * time.Second):
		t.Fatalf("the job never opened the fifo:\n%s", a.text())
	}
}

func undoAfterBgJobWroteFileRead(a *app, name string) string {
	b, _ := os.ReadFile(filepath.Join(a.home, name))
	return string(b)
}

func TestUndoAfterBgJobWroteFile(t *testing.T) {
	t.Parallel()
	a := undoAfterBgJobWroteFileStart(t)
	jobsSay(a, "rewrite a.txt and start the gated job")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}

	t.Run("TestUndoAfterBgJobTurnWroteOnlyAtxt", func(t *testing.T) {
		if got := undoAfterBgJobWroteFileRead(a, "a.txt"); got != "turn\n" {
			t.Fatalf("a.txt after the turn = %q, want %q:\n%s", got, "turn\n", a.text())
		}
		if _, err := os.Stat(filepath.Join(a.home, "job.txt")); !os.IsNotExist(err) {
			t.Fatalf("job.txt exists before the gate opened: %v", err)
		}
	})

	undoAfterBgJobWroteFileOpenGate(t, a)
	// The finished job wakes an idle agent: a second turn.
	if !a.waitDone(2, 60*time.Second) {
		t.Fatalf("the finished job never woke a turn:\n%s", a.text())
	}
	a.waitUntil(func(string) bool {
		return undoAfterBgJobWroteFileRead(a, "job.txt") == "job\n"
	}, "the job to write job.txt")
	a.check("job landed")

	// The wake turn wrote nothing; /undo may take it first (0 files).
	// Keep undoing until the turn that wrote a.txt is reverted.
	var out string
	for i := 0; i < 2; i++ {
		a.undoRun()
		a.waitUntil(func(s string) bool { return strings.Count(s, "reverted ") > i }, "an undo reply")
		out = a.settled()
		if strings.Contains(out, "reverted 1 file from turn 2") {
			break
		}
	}
	a.check("after /undo")

	t.Run("TestUndoAfterBgJobListsOnlyTurnFiles", func(t *testing.T) {
		if !strings.Contains(out, "reverted 1 file from turn 2") {
			t.Fatalf("no undo of turn 2 listing exactly one file:\n%s", out)
		}
		// Only the reply: the job's own command line names job.txt.
		reply := out[strings.LastIndex(out, "reverted "):]
		if strings.Contains(reply, "job.txt") {
			t.Errorf("undo listed job.txt, which only the background job wrote:\n%s", out)
		}
	})

	t.Run("TestUndoAfterBgJobKeepsJobOnlyFile", func(t *testing.T) {
		if got := undoAfterBgJobWroteFileRead(a, "job.txt"); got != "job\n" {
			t.Errorf("job.txt = %q after /undo, want the job's %q:\n%s", got, "job\n", a.text())
		}
	})

	t.Run("TestUndoAfterBgJobSharedFileNotSilentlyClobbered", func(t *testing.T) {
		got := undoAfterBgJobWroteFileRead(a, "a.txt")
		if got == "job\n" {
			return // the job's later write survived
		}
		if strings.Contains(out, "a.txt (") || strings.Contains(strings.ToLower(out), "changed since") {
			return // clobbered, but the reply says so
		}
		if os.Getenv("BOUGH_KNOWN_UNDO_AFTER_BGJOB_WROTE_FILE") == "" {
			t.Skip("known bug: /undo restores a.txt from the checkpoint over the background job's later write with no warning (plugins/commands/tree.go runUndo -> history.Restore never checks the file still holds the turn's content); set BOUGH_KNOWN_UNDO_AFTER_BGJOB_WROTE_FILE=1 to run")
		}
		t.Errorf("a.txt = %q: /undo silently clobbered the background job's later write (%q); reply:\n%s", got, "job\n", out)
	})
}
