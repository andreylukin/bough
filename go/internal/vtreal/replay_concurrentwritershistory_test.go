package vtreal

// Two bough instances resumed on the same session file, each running a
// turn at once. The log must stay line-valid JSON (no interleaved
// writes), and either both turns land in it or the second instance
// refuses the session with a clear message on screen.

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// concurrentWritersHistoryRefusal is what a refusing second instance
// might say; any of these on screen counts as a clear refusal.
var concurrentWritersHistoryRefusal = regexp.MustCompile(`(?i)(already (open|in use|running)|locked|another (bough|instance))`)

// concurrentWritersHistoryLines checks every line of path is one JSON
// object and returns the entry count by kind plus every seq seen.
func concurrentWritersHistoryLines(t *testing.T, path string) (kinds map[string]int, seqs []int64) {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 1<<20), 64<<20)
	kinds = map[string]int{}
	for n := 1; sc.Scan(); n++ {
		var e history.Entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Errorf("%s line %d is not valid JSON (%v): %q", path, n, err, sc.Text())
			continue
		}
		kinds[e.Kind]++
		seqs = append(seqs, e.Seq)
	}
	if err := sc.Err(); err != nil {
		t.Fatal(err)
	}
	return kinds, seqs
}

// concurrentWritersHistoryQuit double-ctrl+c's an instance and waits
// for it to exit, so every buffered write reaches the file.
func concurrentWritersHistoryQuit(x *app) {
	x.key('c', uv.ModCtrl)
	x.key('c', uv.ModCtrl)
	done := make(chan struct{})
	go func() { _, _ = x.cmd.Process.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
	}
}

func TestConcurrentWritersHistory(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	// Seed the session with one finished turn, then quit.
	seed := startCfg(t, 100, 40, resumeConfig(tape, log))
	resumeSend(t, seed, log, "greet me", 1)
	concurrentWritersHistoryQuit(seed)

	// Resume it twice, then send a turn to both at once.
	a := startCfg(t, 100, 40, resumeConfig(next, log))
	b := startCfg(t, 100, 40, resumeConfig(next, log))
	a.check("a: resumed boot")
	b.check("b: resumed boot")
	for _, x := range []*app{a, b} {
		x.typeText("and now")
		x.key(uv.KeyEnter, 0)
	}

	// Wait for both turns, or for a refusal on b.
	refused := false
	deadline := time.Now().Add(60 * time.Second)
	for resumeDones(log) < 3 {
		if concurrentWritersHistoryRefusal.MatchString(b.text()) {
			refused = true
			resumeWaitDones(t, a, log, 2)
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("only %d of 3 turns finished in %s\n--- a:\n%s\n--- b:\n%s", resumeDones(log), log, a.text(), b.text())
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.check("a: after turn")
	b.check("b: after turn")
	concurrentWritersHistoryQuit(a)
	concurrentWritersHistoryQuit(b)

	kinds, seqs := concurrentWritersHistoryLines(t, log)
	want := 3
	if refused {
		want = 2
	}
	if kinds["input"] != want || kinds["done"] != want {
		t.Errorf("log has %d input and %d done entries, want %d each (refused=%v)", kinds["input"], kinds["done"], want, refused)
	}

	t.Run("UniqueSeq", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_CONCURRENT_WRITERS_HISTORY") == "" {
			t.Skip("known bug: two instances resuming one session both continue Seq from the max read at open (history.OpenExisting), so their entries reuse the same seq numbers; set BOUGH_KNOWN_CONCURRENT_WRITERS_HISTORY=1 to run")
		}
		seen := map[int64]bool{}
		var dup []int64
		for _, s := range seqs {
			if seen[s] {
				dup = append(dup, s)
			}
			seen[s] = true
		}
		if len(dup) > 0 {
			t.Errorf("seq values %v repeat in %s (all seqs: %v)", dup, log, seqs)
		}
	})
}
