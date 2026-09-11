package vtreal

// Soak: many replayed turns with seeded churn between them — resizes,
// the history inspector, the todo strip, the action palette, the
// thinking cycle — and short idle stretches. Guards drift over a long
// life: RSS and thread count must plateau, the frame must settle as
// fast at the end as at the start, the idle screen must not redraw,
// and nothing may pile up in the scratch or temp dirs.
//
// bough has no debug endpoint, so goroutines are approximated by the
// process's OS thread count (ps), RSS by ps too.
//
// 40 turns by default; the 500-turn soak is env-gated:
//
//	BOUGH_SOAK_IDLE_AND_CHURN=1 go test ./internal/vtreal -run TestSoakIdleAndChurn -timeout 60m

import (
	"fmt"
	"math/rand"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// soakidleandchurnStart is startCfg with TMPDIR inside the run's $HOME,
// so temp files the binary leaves behind can be counted.
func soakidleandchurnStart(t *testing.T, cols, rows int, yml string) (*app, string) {
	t.Helper()
	home := t.TempDir()
	tmp := filepath.Join(home, "tmp")
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TMPDIR="+tmp, "TERM=xterm-256color", "COLORTERM=truecolor",
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
	return a, tmp
}

// soakidleandchurnSample returns the process's RSS in KiB and its OS
// thread count.
func soakidleandchurnSample(t *testing.T, pid int) (rss, threads int) {
	t.Helper()
	out, err := exec.Command("ps", "-o", "rss=", "-p", strconv.Itoa(pid)).Output()
	if err != nil {
		t.Fatalf("ps rss: %v", err)
	}
	rss, _ = strconv.Atoi(strings.TrimSpace(string(out)))
	if runtime.GOOS == "linux" {
		b, _ := os.ReadFile(fmt.Sprintf("/proc/%d/status", pid))
		for _, l := range strings.Split(string(b), "\n") {
			if v, ok := strings.CutPrefix(l, "Threads:"); ok {
				threads, _ = strconv.Atoi(strings.TrimSpace(v))
			}
		}
		return rss, threads
	}
	out, err = exec.Command("ps", "-M", "-p", strconv.Itoa(pid)).Output()
	if err != nil {
		t.Fatalf("ps -M: %v", err)
	}
	return rss, len(strings.Split(strings.TrimSpace(string(out)), "\n")) - 1
}

// soakidleandchurnFiles counts regular files under dir.
func soakidleandchurnFiles(dir string) int {
	n := 0
	_ = filepath.WalkDir(dir, func(_ string, d os.DirEntry, err error) error {
		if err == nil && !d.IsDir() {
			n++
		}
		return nil
	})
	return n
}

func soakidleandchurnMedian(ds []time.Duration) time.Duration {
	s := slices.Clone(ds)
	slices.Sort(s)
	return s[len(s)/2]
}

// soakidleandchurnChurn does one random UI action and returns its name.
func soakidleandchurnChurn(a *app, rng *rand.Rand) string {
	// Resize is not churned here: x/vt drops rows on resize, so it
	// soaks under tmux in TestSoakIdleAndChurnResizeTmux.
	switch rng.Intn(4) {
	case 0:
		a.keymapCtrl('o')
		a.waitFor("inspecting · ctrl+o to close")
		a.keymapCtrl('o')
		a.keymapGone("inspecting ·")
		return "inspect toggle"
	case 1:
		a.keymapCtrl('t')
		a.settled()
		a.keymapCtrl('t')
		return "todo toggle"
	case 2:
		a.keymapChord('p')
		a.waitUntil(func(string) bool { return strings.HasPrefix(a.keymapComposer(), "> /") }, "palette open")
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(string) bool { return strings.Contains(a.keymapComposer(), "say something") }, "palette closed")
		return "palette"
	default:
		a.key(uv.KeyTab, uv.ModShift)
		return "think cycle"
	}
}

func soakidleandchurnRun(t *testing.T, n int) {
	tape := longSessionTape(t, n)
	a, tmp := soakidleandchurnStart(t, 100, 30, replayConfig(tape))
	pid := a.cmd.Process.Pid
	rng := rand.New(rand.NewSource(20260911))
	scratch := filepath.Join(a.home, ".bough", "scratch")
	a.check("boot")

	warm := max(n/10, 5)
	var settle []time.Duration
	var baseRSS, baseThreads, baseFiles, peakRSS, peakThreads int
	for i := 1; i <= n; i++ {
		var did []string
		for range rng.Intn(3) {
			did = append(did, soakidleandchurnChurn(a, rng))
		}
		where := fmt.Sprintf("turn %d/%d after %v", i, n, did)
		a.typeText(fmt.Sprintf("do step %d", i))
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(i, 60*time.Second) {
			t.Fatalf("%s: turn never finished:\n%s", where, a.text())
		}
		start := time.Now()
		screen := a.settled()
		settle = append(settle, time.Since(start))
		if !strings.Contains(screen, longSessionMarker(i)) {
			t.Errorf("%s: answer %s not on screen:\n%s", where, longSessionMarker(i), screen)
		}
		a.check(where)
		if t.Failed() {
			return
		}
		if i%25 == 0 { // idle: a settled screen must not redraw
			base := a.text()
			for range 10 {
				time.Sleep(100 * time.Millisecond)
				if cur := a.text(); cur != base {
					t.Fatalf("%s: screen redrew while idle:\nbefore:\n%s\nafter:\n%s", where, base, cur)
				}
			}
		}
		rss, th := soakidleandchurnSample(t, pid)
		if i == warm {
			baseRSS, baseThreads = rss, th
			baseFiles = soakidleandchurnFiles(scratch) + soakidleandchurnFiles(tmp)
		}
		if i > warm {
			peakRSS, peakThreads = max(peakRSS, rss), max(peakThreads, th)
		}
		if i == 1 || i == warm || i == n || i%50 == 0 {
			t.Logf("turn %d: rss %d KiB, threads %d, settle %s", i, rss, th, settle[i-1].Round(time.Millisecond))
		}
	}

	// Plateau: after warm-up, RSS may grow with the transcript but not
	// without bound; threads must stay flat.
	if limit := baseRSS*2 + 64*1024; peakRSS > limit {
		t.Errorf("RSS did not plateau: %d KiB at turn %d, peak %d KiB after (limit %d)", baseRSS, warm, peakRSS, limit)
	}
	if limit := baseThreads + 8; peakThreads > limit {
		t.Errorf("threads did not plateau: %d at turn %d, peak %d after (limit %d)", baseThreads, warm, peakThreads, limit)
	}
	// Frame time: the last turns settle within 2x of the first (medians
	// of a window, plus one settle poll of slack for quantisation).
	w := min(10, n/4)
	first, last := soakidleandchurnMedian(settle[:w]), soakidleandchurnMedian(settle[n-w:])
	t.Logf("settle median: first %d turns %s, last %d turns %s", w, first, w, last)
	if last > 2*first+60*time.Millisecond {
		t.Errorf("frame settle drifted: first %s, last %s (> 2x)", first, last)
	}
	if files := soakidleandchurnFiles(scratch) + soakidleandchurnFiles(tmp); files > baseFiles {
		t.Errorf("temp files leaked: %d in scratch+TMPDIR at turn %d, %d at the end", baseFiles, warm, files)
	}
}

func TestSoakIdleAndChurnShort(t *testing.T) {
	t.Parallel()
	soakidleandchurnRun(t, 40)
}

func TestSoakIdleAndChurnFull(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_IDLE_AND_CHURN") == "" {
		t.Skip("set BOUGH_SOAK_IDLE_AND_CHURN=1 to run the 500-turn soak")
	}
	soakidleandchurnRun(t, 500)
}

// Resize churn under tmux (x/vt drops rows on resize): seeded resizes
// between replayed turns; the bough process must keep RSS and threads
// flat and every size must hold the composer and status bar. 30 turns
// by default, 500 with BOUGH_SOAK_IDLE_AND_CHURN=1.
func TestSoakIdleAndChurnResizeTmux(t *testing.T) {
	t.Parallel()
	n := 30
	if os.Getenv("BOUGH_SOAK_IDLE_AND_CHURN") != "" {
		n = 500
	}
	tm, home := resizeTmuxStart(t, 100, 30, replayConfig(longSessionTape(t, n)))
	shell := strings.TrimSpace(tm.run("display", "-p", "-t", "0", "#{pane_pid}"))
	// The pane's shell usually execs bough (last command), so the pane
	// pid is bough itself; otherwise bough is its child.
	pid := 0
	tm.waitUntil(func(string) bool {
		for _, args := range [][]string{{"ps", "-o", "pid=,comm=", "-p", shell}, {"pgrep", "-l", "-P", shell}} {
			out, _ := exec.Command(args[0], args[1:]...).Output()
			if f := strings.Fields(string(out)); len(f) >= 2 && strings.Contains(f[1], "bough") {
				pid, _ = strconv.Atoi(f[0])
				return true
			}
		}
		return false
	}, "the bough process of the tmux pane")
	rng := rand.New(rand.NewSource(20260911))
	sizes := [][2]int{{100, 30}, {80, 24}, {140, 40}, {60, 12}, {200, 50}}
	warm := max(n/10, 5)
	var baseRSS, baseThreads, peakRSS, peakThreads int
	for i := 1; i <= n; i++ {
		sz := sizes[rng.Intn(len(sizes))]
		tm.resize(sz[0], sz[1])
		where := fmt.Sprintf("turn %d/%d at %dx%d", i, n, sz[0], sz[1])
		tm.waitUntil(func(s string) bool {
			ls := strings.Split(s, "\n")
			return composerRow(ls) >= len(ls)-2 && strings.Contains(s, "? keys")
		}, where+": composer + status bar on the last rows")
		tm.keys(fmt.Sprintf("do step %d", i), "Enter")
		tm.waitUntil(func(string) bool { return resizeTmuxDone(home) >= i }, where+": turn done")
		if s := tm.settled(); panicky.MatchString(s) {
			t.Fatalf("%s: crash text on screen:\n%s", where, s)
		}
		rss, th := soakidleandchurnSample(t, pid)
		if i == warm {
			baseRSS, baseThreads = rss, th
		}
		if i > warm {
			peakRSS, peakThreads = max(peakRSS, rss), max(peakThreads, th)
		}
		if i == 1 || i == warm || i == n || i%50 == 0 {
			t.Logf("turn %d: rss %d KiB, threads %d", i, rss, th)
		}
	}
	if limit := baseRSS*2 + 64*1024; peakRSS > limit {
		t.Errorf("RSS did not plateau under resize: %d KiB at turn %d, peak %d KiB (limit %d)", baseRSS, warm, peakRSS, limit)
	}
	if limit := baseThreads + 8; peakThreads > limit {
		t.Errorf("threads did not plateau under resize: %d at turn %d, peak %d (limit %d)", baseThreads, warm, peakThreads, limit)
	}
}
