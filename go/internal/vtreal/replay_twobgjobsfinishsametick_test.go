package vtreal

// Two background jobs that finish on the same tick while the agent is
// idle. Turn 1 detaches both, each polling one flag file; the test
// creates that file once, which releases both together. (A fifo with
// two readers cannot promise that: the writer's open succeeds on the
// first reader alone and the second blocks forever.) The loop may land
// both in one wake turn, land the second inside the first wake turn
// (landJobs), or run two sequential wakes — but never two turns at
// once, never a job twice, never a job dropped.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// twobgjobsfinishsametickTape writes the tape: one block starting two
// jobs, then two wake replies (the second goes unused when both
// notices share one wake).
func twobgjobsfinishsametickTape(t *testing.T, cmdA, cmdB string) string {
	t.Helper()
	code := fmt.Sprintf("console.log(tools.bash(%q, 120))\nconsole.log(tools.bash(%q, 120))\n", cmdA, cmdB)
	rows := []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "start both gated jobs"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"code", map[string]any{"text": code}},
		{"result", map[string]any{"code": code, "text": "job 1 started\njob 2 started\n"}},
		{"assistant", map[string]any{"text": "```stop\nStarted jobs 1 and 2.\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": "[background job] wake one"}},
		{"assistant", map[string]any{"text": "```stop\nWAKE-REPLY-ONE\n```"}},
		{"done", map[string]any{"text": ""}},
		{"input", map[string]any{"text": "[background job] wake two"}},
		{"assistant", map[string]any{"text": "```stop\nWAKE-REPLY-TWO\n```"}},
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
	p := filepath.Join(t.TempDir(), "two-bgjobs.jsonl")
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestTwoBgjobsFinishSameTick(t *testing.T) {
	t.Parallel()
	dir, err := os.MkdirTemp("", "twobg") // short: the path rides in the commands
	if err != nil {
		t.Fatal(err)
	}
	flag := filepath.Join(dir, "go")
	t.Cleanup(func() { os.Remove(flag); os.Remove(dir) })
	gate := fmt.Sprintf("while [ ! -e %s ]; do sleep 0.02; done; ", flag)
	cmdA, cmdB := gate+"echo TWIN-A-DONE", gate+"echo TWIN-B-DONE"
	a := startCfg(t, 100, 40, jobsConfig(twobgjobsfinishsametickTape(t, cmdA, cmdB)))
	a.check("boot")

	jobsSay(a, "start both gated jobs")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn 1 never finished:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return strings.Contains(s, "job 2") }, "the job strip to name job 2")
	time.Sleep(500 * time.Millisecond) // idle, both jobs polling
	if a.doneCount() != 1 {
		t.Fatalf("a wake turn ran before the gate opened:\n%s", a.text())
	}
	if err := os.WriteFile(flag, nil, 0o600); err != nil { // one write releases both
		t.Fatal(err)
	}

	// Wait for both jobs' output to reach the model: a wake input or a
	// job landed inside a turn.
	deadline := time.Now().Add(60 * time.Second)
	for {
		var all strings.Builder
		for _, e := range jobsHistory(a) {
			if text, _ := e.Data["text"].(string); e.Kind == "input" || e.Kind == "job" {
				all.WriteString(text)
			}
		}
		s := all.String()
		if strings.Contains(s, "\nTWIN-A-DONE") && strings.Contains(s, "\nTWIN-B-DONE") {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("both jobs never reached the model:\n%s\nhistory:\n%s", a.text(), twobgjobsfinishsametickDump(jobsHistory(a)))
		}
		time.Sleep(100 * time.Millisecond)
	}
	time.Sleep(2 * time.Second) // let any stray extra wake show itself
	wakes := 0
	for _, e := range jobsHistory(a) {
		if text, _ := e.Data["text"].(string); e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
			wakes++
		}
	}
	if !a.waitDone(1+wakes, 60*time.Second) {
		t.Fatalf("the wake turns never finished (%d wakes):\n%s", wakes, a.text())
	}
	es := jobsHistory(a)

	t.Run("each job reaches the model once", func(t *testing.T) {
		if wakes < 1 || wakes > 2 {
			t.Errorf("want 1 or 2 wake inputs, got %d", wakes)
		}
		for _, mark := range []string{"TWIN-A-DONE", "TWIN-B-DONE"} {
			n := 0
			for _, e := range es {
				text, _ := e.Data["text"].(string)
				if e.Kind == "input" || e.Kind == "job" {
					n += strings.Count(text, "\n"+mark) // the output line, not the echo in the command
				}
			}
			if n != 1 {
				t.Errorf("%s reached the model %d times, want 1", mark, n)
			}
		}
		seen := map[string]bool{}
		for _, e := range es {
			if text, _ := e.Data["text"].(string); e.Kind == "input" && strings.HasPrefix(text, "[background job] ") {
				if seen[text] {
					t.Errorf("duplicate wake input: %q", text)
				}
				seen[text] = true
			}
		}
	})

	t.Run("wake turns never overlap", func(t *testing.T) {
		open := false
		for i, e := range es {
			switch e.Kind {
			case "input":
				if open {
					t.Errorf("input at entry %d starts while a turn is still open", i)
				}
				open = true
			case "done", "cancelled":
				open = false
			}
		}
	})

	t.Run("notes rendered once each", func(t *testing.T) {
		if n := a.doneCount(); n != 1+wakes {
			t.Errorf("want %d finished turns, got %d:\n%s", 1+wakes, n, a.text())
		}
		s := a.settled()
		for _, head := range []string{"job 1 [exited 0]", "job 2 [exited 0]"} {
			if n := strings.Count(s, "▸ job (2 lines): "+head); n != 1 {
				t.Errorf("want one collapsed %q note on screen, got %d:\n%s", head, n, s)
			}
		}
		if strings.Contains(s, "[background job]") {
			t.Errorf("wake preamble leaked onto the screen:\n%s", s)
		}
		a.check("after wakes")
	})
}

// twobgjobsfinishsametickDump renders history entries for a failure.
func twobgjobsfinishsametickDump(es []history.Entry) string {
	var b strings.Builder
	for _, e := range es {
		text, _ := e.Data["text"].(string)
		fmt.Fprintf(&b, "%s %q\n", e.Kind, text)
	}
	return b.String()
}
