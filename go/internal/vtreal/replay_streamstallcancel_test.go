package vtreal

// A stream that stalls mid-delta: the replay llm's delay is set to an
// hour, so after the first word the provider goes silent for good (a
// hung connection that never errors). The spinner must keep ticking,
// esc must cancel within a second, the partial text must stay (or be
// marked cancelled), the history file must stay valid JSON lines, and
// the next prompt must still get a turn.

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// streamStallCancelDelayMS is long enough that no test outlives it.
const streamStallCancelDelayMS = 3_600_000

func TestStreamStallCancel(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, cancelConfig(cancelTape(t), streamStallCancelDelayMS))

	a.typeText("start the long one")
	a.key(uv.KeyEnter, 0)
	a.waitFor("ALPHASTART")

	// The stream is stalled: the spinner must keep animating and the
	// elapsed chip must advance even though no delta arrives.
	first := cancelSpinner.FindString(a.text())
	if first == "" {
		t.Fatalf("no spinner while the stream is stalled:\n%s", a.text())
	}
	frames := map[string]bool{first: true}
	deadline := time.Now().Add(2500 * time.Millisecond)
	for time.Now().Before(deadline) {
		if m := cancelSpinner.FindString(a.text()); m != "" {
			frames[m] = true
		}
		time.Sleep(30 * time.Millisecond)
	}
	if len(frames) < 3 {
		t.Fatalf("spinner froze during the stall (frames seen: %v):\n%s", frames, a.text())
	}
	if s := a.text(); strings.Contains(s, "filler") {
		t.Fatalf("the stalled stream kept delivering words:\n%s", s)
	}

	// Esc cancels within a second.
	pressed := time.Now()
	a.key(uv.KeyEsc, 0)
	var s string
	for {
		s = a.text()
		if strings.Contains(s, "■ cancelled") && !cancelSpinner.MatchString(s) {
			break
		}
		if time.Since(pressed) > time.Second {
			t.Fatalf("esc did not cancel the stalled stream within 1s:\n%s", s)
		}
		time.Sleep(10 * time.Millisecond)
	}

	s = a.settled()
	if !strings.Contains(s, "ALPHASTART") {
		t.Fatalf("partial text dropped after the cancel:\n%s", s)
	}
	if strings.Contains(s, "ALPHAEND") {
		t.Fatalf("the reply finished despite the stall and cancel:\n%s", s)
	}
	a.check("after stalled cancel")
	if !a.waitDone(1, 5*time.Second) {
		t.Fatalf("no done/cancelled entry in history after the cancel")
	}
	streamStallCancelHistoryValid(t, a.home)

	// The next prompt still gets a turn: the tape's second reply
	// streams its first word ("```stop") and then stalls too, so the
	// proof is a new running turn that esc ends as well.
	a.typeText("second")
	a.key(uv.KeyEnter, 0)
	a.waitFor("❯ second")
	a.waitUntil(func(s string) bool { return cancelSpinner.MatchString(s) },
		"the spinner to start on the next turn")
	a.key(uv.KeyEsc, 0)
	a.waitUntil(func(s string) bool { return !cancelSpinner.MatchString(s) },
		"the spinner to stop after the second cancel")
	if !a.waitDone(2, 5*time.Second) {
		t.Fatalf("second turn left no done/cancelled entry")
	}
	a.check("after the next turn")
	streamStallCancelHistoryValid(t, a.home)
}

// streamStallCancelHistoryValid checks every history line under home
// is one complete JSON object.
func streamStallCancelHistoryValid(t *testing.T, home string) {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	if len(paths) == 0 {
		t.Fatalf("no history file under %s", home)
	}
	for _, p := range paths {
		f, err := os.Open(p)
		if err != nil {
			t.Fatal(err)
		}
		sc := bufio.NewScanner(f)
		sc.Buffer(make([]byte, 1<<20), 1<<24)
		for n := 1; sc.Scan(); n++ {
			if line := sc.Bytes(); len(line) > 0 && !json.Valid(line) {
				t.Errorf("%s line %d is not valid JSON: %q", p, n, line)
			}
		}
		if err := sc.Err(); err != nil {
			t.Errorf("%s: %v", p, err)
		}
		f.Close()
	}
}
