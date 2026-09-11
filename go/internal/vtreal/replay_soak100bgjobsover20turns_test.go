package vtreal

// Soak: five short background jobs per turn for twenty turns, each
// finishing at a seeded offset. Every turn's block starts its five
// jobs, then the closing reply streams for longer than the largest
// offset, so all five finish while the agent is still busy and none
// is landed mid-step: the idle period after the turn's done must open
// exactly one wake turn carrying all five notices — never zero, never
// one per job. At the end: no job left in the strip, no child process
// left behind, the fd table back to its warmed-up size, and RSS under
// a bound.
//
// The three-turn run is always on; the full 100-job soak runs under
// BOUGH_SOAK_100BGJOBS.

import (
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

const (
	soak100bgjobsover20turnsSeed    = 20260911
	soak100bgjobsover20turnsPerTurn = 5
	soak100bgjobsover20turnsDelayMS = 40
	// 70 words at 40ms is ~2.8s of streaming; offsets top out at 1.2s.
	soak100bgjobsover20turnsWords = 70
)

// soak100bgjobsover20turnsMarker is job j of turn k's output line.
func soak100bgjobsover20turnsMarker(k, j int) string {
	return fmt.Sprintf("SOAK-T%02d-J%d", k, j)
}

// soak100bgjobsover20turnsOffsets is the seeded finish offsets, in
// milliseconds, 200..1200, one row per turn.
func soak100bgjobsover20turnsOffsets(turns int) [][]int {
	r := rand.New(rand.NewPCG(soak100bgjobsover20turnsSeed, 0))
	out := make([][]int, turns)
	for k := range out {
		for range soak100bgjobsover20turnsPerTurn {
			out[k] = append(out[k], 200+r.IntN(1001))
		}
	}
	return out
}

// soak100bgjobsover20turnsTape writes the tape: per turn, a block that
// starts five jobs, a long closing reply, and the wake turn's reply.
func soak100bgjobsover20turnsTape(t *testing.T, offsets [][]int) string {
	t.Helper()
	words := make([]string, soak100bgjobsover20turnsWords)
	for i := range words {
		words[i] = fmt.Sprintf("s%02d", i)
	}
	var b strings.Builder
	seq := 0
	write := func(kind, text string) {
		seq++
		line, err := json.Marshal(map[string]any{"seq": seq, "at": "2026-09-11T10:00:00Z", "kind": kind, "data": map[string]any{"text": text}})
		if err != nil {
			t.Fatal(err)
		}
		b.Write(line)
		b.WriteByte('\n')
	}
	for k, row := range offsets {
		var code strings.Builder
		for j, ms := range row {
			cmd := fmt.Sprintf("sleep %d.%03d; echo %s", ms/1000, ms%1000, soak100bgjobsover20turnsMarker(k+1, j+1))
			fmt.Fprintf(&code, "console.log(tools.bash(%q, 60))\n", cmd)
		}
		write("input", fmt.Sprintf("soak turn %d", k+1))
		write("assistant", "```js\n"+code.String()+"```")
		write("assistant", fmt.Sprintf("```stop\nTURN-%02d-STARTED %s\n```", k+1, strings.Join(words, " ")))
		write("done", "")
		write("input", "[background job] wake")
		write("assistant", fmt.Sprintf("```stop\nWAKE-%02d-OK\n```", k+1))
		write("done", "")
	}
	p := filepath.Join(t.TempDir(), "soak-bgjobs.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// soak100bgjobsover20turnsFDs counts the open descriptors of pid.
func soak100bgjobsover20turnsFDs(t *testing.T, pid int) int {
	t.Helper()
	if runtime.GOOS == "linux" {
		es, err := os.ReadDir(fmt.Sprintf("/proc/%d/fd", pid))
		if err != nil {
			t.Fatal(err)
		}
		return len(es)
	}
	out, err := exec.Command("lsof", "-n", "-P", "-p", strconv.Itoa(pid)).Output()
	if err != nil {
		t.Fatalf("lsof: %v", err)
	}
	n := 0
	for _, l := range strings.Split(string(out), "\n")[1:] {
		f := strings.Fields(l)
		// Numbered descriptors only (3u, 12r …): cwd/txt/mem rows are
		// mappings, not fds.
		if len(f) > 3 && f[3] != "" && f[3][0] >= '0' && f[3][0] <= '9' {
			n++
		}
	}
	return n
}

// soak100bgjobsover20turnsRSS is pid's resident set in KiB.
func soak100bgjobsover20turnsRSS(t *testing.T, pid int) int {
	t.Helper()
	out, err := exec.Command("ps", "-o", "rss=", "-p", strconv.Itoa(pid)).Output()
	if err != nil {
		t.Fatalf("ps rss: %v", err)
	}
	n, err := strconv.Atoi(strings.TrimSpace(string(out)))
	if err != nil {
		t.Fatalf("ps rss %q: %v", out, err)
	}
	return n
}

// soak100bgjobsover20turnsChildren lists pid's child processes,
// zombies included.
func soak100bgjobsover20turnsChildren(t *testing.T, pid int) []string {
	t.Helper()
	out, err := exec.Command("ps", "-A", "-o", "ppid=,pid=,stat=,command=").Output()
	if err != nil {
		t.Fatalf("ps: %v", err)
	}
	var kids []string
	for _, l := range strings.Split(string(out), "\n") {
		f := strings.Fields(l)
		if len(f) > 1 && f[0] == strconv.Itoa(pid) {
			kids = append(kids, strings.TrimSpace(l))
		}
	}
	return kids
}

func soak100bgjobsover20turnsRun(t *testing.T, turns int) {
	offsets := soak100bgjobsover20turnsOffsets(turns)
	t.Logf("seed %d offsets(ms) %v", soak100bgjobsover20turnsSeed, offsets)
	tape := soak100bgjobsover20turnsTape(t, offsets)
	a := startCfg(t, 100, 30, bgjobfinishesmidturnConfig(tape, soak100bgjobsover20turnsDelayMS))
	a.check("boot")
	pid := a.cmd.Process.Pid

	var fd0, rss0 int
	for k := 1; k <= turns; k++ {
		jobsSay(a, fmt.Sprintf("soak turn %d", k))
		if !a.waitDone(2*k-1, 60*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", k, a.text())
		}
		if !a.waitDone(2*k, 60*time.Second) {
			t.Fatalf("turn %d's jobs never woke a turn:\n%s", k, a.text())
		}
		a.waitFor(fmt.Sprintf("WAKE-%02d-OK", k))
		if k == 1 {
			// Warmed up: codemode, the job table and the strip have all
			// been through one full cycle.
			time.Sleep(300 * time.Millisecond)
			fd0, rss0 = soak100bgjobsover20turnsFDs(t, pid), soak100bgjobsover20turnsRSS(t, pid)
			t.Logf("after turn 1: fds %d rss %d KiB", fd0, rss0)
		}
	}
	a.check("after soak")

	t.Run("one wake per idle period", func(t *testing.T) {
		// A spurious wake would show up as a done past 2*turns.
		time.Sleep(2 * time.Second)
		if n := a.doneCount(); n != 2*turns {
			t.Errorf("want %d finished turns (one wake per turn), got %d", 2*turns, n)
		}
		es := jobsHistory(a)
		turn, wakes := 0, 0
		seen := map[string]int{}
		check := func() {
			if turn > 0 && wakes != 1 {
				t.Errorf("idle period after turn %d opened %d wake turns, want 1", turn, wakes)
			}
		}
		for _, e := range es {
			text, _ := e.Data["text"].(string)
			if e.Kind != "input" {
				continue
			}
			if !strings.HasPrefix(text, "[background job] ") {
				check()
				turn++
				wakes = 0
				continue
			}
			wakes++
			for j := 1; j <= soak100bgjobsover20turnsPerTurn; j++ {
				m := soak100bgjobsover20turnsMarker(turn, j)
				if !strings.Contains(text, m) {
					t.Errorf("wake after turn %d is missing %s:\n%s", turn, m, text)
				}
			}
			for k := 1; k <= turns; k++ {
				for j := 1; j <= soak100bgjobsover20turnsPerTurn; j++ {
					m := soak100bgjobsover20turnsMarker(k, j)
					seen[m] += strings.Count(text, "echo "+m)
				}
			}
		}
		check()
		if turn != turns {
			t.Errorf("history holds %d user turns, want %d", turn, turns)
		}
		for m, n := range seen {
			if n > 1 {
				t.Errorf("%s reported %d times", m, n)
			}
		}
	})

	t.Run("job strip empty", func(t *testing.T) {
		ls := a.lines()
		c := composerRow(ls)
		if c < 0 {
			t.Fatalf("no composer on screen:\n%s", a.text())
		}
		for _, l := range ls[c+1:] {
			if strings.Contains(l, "SOAK-T") || strings.Contains(l, "tools.jobs") {
				t.Errorf("job strip still shows %q:\n%s", l, a.text())
			}
		}
	})

	t.Run("no leaked children or fds", func(t *testing.T) {
		if kids := soak100bgjobsover20turnsChildren(t, pid); len(kids) > 0 {
			t.Errorf("bough still has %d child processes:\n%s", len(kids), strings.Join(kids, "\n"))
		}
		fd := soak100bgjobsover20turnsFDs(t, pid)
		t.Logf("end: fds %d (after turn 1: %d)", fd, fd0)
		if fd > fd0+8 {
			t.Errorf("fd table grew from %d to %d over %d jobs", fd0, fd, turns*soak100bgjobsover20turnsPerTurn)
		}
	})

	t.Run("rss bounded", func(t *testing.T) {
		rss := soak100bgjobsover20turnsRSS(t, pid)
		t.Logf("end: rss %d KiB (after turn 1: %d)", rss, rss0)
		if rss > 512*1024 {
			t.Errorf("rss %d KiB is over 512 MiB", rss)
		}
		if rss > rss0+128*1024 {
			t.Errorf("rss grew from %d to %d KiB over %d jobs", rss0, rss, turns*soak100bgjobsover20turnsPerTurn)
		}
	})
}

// Three turns, fifteen jobs: the same invariants on every run.
func TestSoak100BgjobsOver20TurnsShort(t *testing.T) {
	t.Parallel()
	soak100bgjobsover20turnsRun(t, 3)
}

// The full soak: twenty turns, a hundred jobs.
func TestSoak100BgjobsOver20Turns(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_100BGJOBS") == "" {
		t.Skip("soak: set BOUGH_SOAK_100BGJOBS=1 to run 100 jobs over 20 turns")
	}
	t.Parallel()
	soak100bgjobsover20turnsRun(t, 20)
}
