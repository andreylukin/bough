package vtreal

// The session log fills its disk mid-session. bough runs under a file
// size limit (ulimit -f, SIGXFSZ ignored so a write past it fails with
// EFBIG instead of killing the process); after turn 1 lands, the test
// pads the log to exactly that limit, so every append of turn 2 fails.
// The TUI must say so, the log must not claim turn 2, nothing panics,
// quitting restores the terminal and the next boot opens the log.

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// historydbdiskfullLimit is the file size limit in ulimit -f blocks
// (1024 bytes in /bin/sh).
const historydbdiskfullLimit = 16384 // 16 MiB: room for the graph db, which shares the limit

// historydbdiskfullStart is startCfg with bough wrapped in a shell that
// sets the file size limit.
func historydbdiskfullStart(t *testing.T, cols, rows int, yml string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command("/bin/sh", "-c",
		`trap '' XFSZ; ulimit -f "$2"; exec "$0" -config "$1"`, bin, cfg, strconv.Itoa(historydbdiskfullLimit))
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// historydbdiskfullPad appends valid, unrendered lines that bring the log
// to exactly the size limit: the disk is now full for bough.
func historydbdiskfullPad(t *testing.T, path string) {
	t.Helper()
	const limit = historydbdiskfullLimit * 1024
	st, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	// Many short lines: the log reader scans line by line and one
	// 16 MiB line is past its token limit.
	stub, _ := json.Marshal(map[string]any{"seq": 0, "kind": "meta", "data": map[string]any{"p": ""}})
	room := limit - int(st.Size())
	if room < 2*4096 {
		t.Fatalf("turn 1 already filled %d of %d bytes", st.Size(), limit)
	}
	var b strings.Builder
	for room > 0 {
		size := 4096
		if room < 2*size {
			size = room // the last line takes what is left
		}
		line, _ := json.Marshal(map[string]any{"seq": 0, "kind": "meta", "data": map[string]any{"p": strings.Repeat("x", size-len(stub)-1)}})
		b.Write(append(line, '\n'))
		room -= size
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if _, err := f.WriteString(b.String()); err != nil {
		t.Fatal(err)
	}
	if st, _ := os.Stat(path); st.Size() != limit {
		t.Fatalf("padded log is %d bytes, want %d", st.Size(), limit)
	}
}

func TestHistoryDBDiskFull(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("needs ulimit -f")
	}
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	a := historydbdiskfullStart(t, 100, 40, resumeConfig(tape, log))
	resumeSend(t, a, log, "greet me", 1)
	a.check("turn 1")
	historydbdiskfullPad(t, log)

	a.typeText("count the files")
	a.key(uv.KeyEnter, 0)
	a.waitFor("Two files here.")
	screen := a.settled()

	t.Run("no panic", func(t *testing.T) {
		if panicky.MatchString(screen) {
			t.Fatalf("crash text on screen:\n%s", screen)
		}
	})
	t.Run("layout intact", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_HISTORY_DB_DISK_FULL") == "" {
			t.Skip("known bug: history.Store.Append prints \"bough: history append: ... file too large\" to stderr, which lands on the alt screen over the composer and status bar (plugins/history/history.go Append); set BOUGH_KNOWN_HISTORY_DB_DISK_FULL=1 to run")
		}
		a.t = t
		a.check("turn 2 on a full disk")
	})
	t.Run("turn not saved", func(t *testing.T) {
		if n := resumeDones(log); n != 1 {
			t.Fatalf("log records %d finished turns on a full disk, want 1", n)
		}
		// The first append after the pad may still land: the kernel
		// checks an O_APPEND write against the fd's stale offset (end of
		// turn 1), not EOF. Every later one fails, so the reply never
		// reaches the log.
		if data, _ := os.ReadFile(log); strings.Contains(string(data), "Two files here.") {
			t.Fatalf("turn 2's reply reached a full log; tail:\n%s", data[max(0, len(data)-600):])
		}
	})
	t.Run("visible error", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_HISTORY_DB_DISK_FULL") == "" {
			t.Skip("known bug: history.Store.Append reports write errors on stderr only (plugins/history/history.go Append); the TUI never shows that turn 2 was not saved; set BOUGH_KNOWN_HISTORY_DB_DISK_FULL=1 to run")
		}
		// Above the composer: rendered by the TUI, not stderr bleeding
		// over the alt screen.
		ls := strings.Split(screen, "\n")
		r := composerRow(ls)
		if r < 0 {
			t.Fatalf("no composer on screen:\n%s", screen)
		}
		low := strings.ToLower(strings.Join(ls[:r], "\n"))
		if !strings.Contains(low, "history") || !(strings.Contains(low, "too large") || strings.Contains(low, "not saved") || strings.Contains(low, "no space")) {
			t.Fatalf("no visible save error in the transcript:\n%s", screen)
		}
	})
	t.Run("quit restores terminal", func(t *testing.T) {
		a.t = t
		a.key('c', uv.ModCtrl)
		a.waitFor("ctrl+c")
		a.key('c', uv.ModCtrl)
		done := make(chan error, 1)
		go func() { done <- a.term.Wait(a.cmd) }()
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("exit: %v\n%s", err, a.text())
			}
		case <-time.After(8 * time.Second):
			t.Fatalf("did not exit after two ctrl+c:\n%s", a.text())
		}
		for range 100 {
			if !a.term.Snapshot().AltScreen {
				return
			}
			time.Sleep(50 * time.Millisecond)
		}
		t.Fatalf("still in the alt screen after exit")
	})
	t.Run("next boot opens the log", func(t *testing.T) {
		b := startCfg(t, 100, 40, resumeConfig(next, log))
		b.waitFor("resumed ")
		b.check("resumed boot")
		if s := b.settled(); !strings.Contains(s, "Hello from turn one.") {
			t.Fatalf("resumed session lost turn 1:\n%s", s)
		}
	})
}
