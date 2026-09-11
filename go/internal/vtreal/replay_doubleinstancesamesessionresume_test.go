package vtreal

// Two bough instances resume the same session id, then each submits a
// turn in sequence (a first, b after a finished). The shared log must
// be a valid tree, linear or forked: unique seqs, every parent a real
// earlier entry, each instance's turn hanging off the resumed session
// rather than off a half of the other's. The second instance should
// also tell its user another bough is writing the session.

import (
	"path/filepath"
	"regexp"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// doubleInstanceSameSessionResumeWarning is any on-screen notice that
// another writer holds the session.
var doubleInstanceSameSessionResumeWarning = regexp.MustCompile(`(?i)(already (open|in use|running)|another (bough|instance|writer)|concurrent|locked)`)

// doubleInstanceSameSessionResumeChain walks e's ancestry and returns
// the kinds from the root down; a missing parent fails the test.
func doubleInstanceSameSessionResumeChain(t *testing.T, bySeq map[int64]history.Entry, e history.Entry) []string {
	t.Helper()
	var kinds []string
	for cur := e; ; {
		kinds = append([]string{cur.Kind}, kinds...)
		p := history.ParentOf(cur)
		if p == 0 {
			return kinds
		}
		next, ok := bySeq[p]
		if !ok {
			t.Errorf("seq %d (%s) has parent %d, which is not in the log", cur.Seq, cur.Kind, p)
			return kinds
		}
		cur = next
	}
}

func TestDoubleInstanceSameSessionResume(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/resume.jsonl")
	next, _ := filepath.Abs("testdata/replay/resume-next.jsonl")
	log := filepath.Join(t.TempDir(), "session.jsonl")

	seed := startCfg(t, 100, 40, resumeConfig(tape, log))
	resumeSend(t, seed, log, "greet me", 1)
	concurrentWritersHistoryQuit(seed)
	seeded, err := history.Read(log)
	if err != nil {
		t.Fatal(err)
	}
	seedLast := seeded[len(seeded)-1].Seq

	// Both resume before either writes; each has its own tape.
	a := startCfg(t, 100, 40, resumeConfig(next, log))
	b := startCfg(t, 100, 40, resumeConfig(next, log))
	a.check("a: resumed boot")
	b.check("b: resumed boot")
	resumeSend(t, a, log, "and now", 2)
	a.check("a: after turn")
	resumeSend(t, b, log, "and now", 3)
	b.check("b: after turn")
	bScreen := b.settled()
	concurrentWritersHistoryQuit(a)
	concurrentWritersHistoryQuit(b)

	kinds, seqs := concurrentWritersHistoryLines(t, log)
	if kinds["input"] != 3 || kinds["done"] != 3 {
		t.Errorf("log has %d input and %d done entries, want 3 each", kinds["input"], kinds["done"])
	}
	entries, err := history.Read(log)
	if err != nil {
		t.Fatal(err)
	}

	t.Run("Tree", func(t *testing.T) {
		bySeq := map[int64]history.Entry{}
		for _, e := range entries {
			if _, dup := bySeq[e.Seq]; dup {
				t.Errorf("seq %d appears twice (all seqs %v)", e.Seq, seqs)
			}
			bySeq[e.Seq] = e
			if p := history.ParentOf(e); p >= e.Seq {
				t.Errorf("seq %d has parent %d, not an earlier entry", e.Seq, p)
			}
		}
		// Each new turn's done: its ancestry passes the seed's last
		// entry, and holds 2 inputs (forked) or 3 (linear, the other
		// turn adopted) — never a partial turn of the other instance.
		var dones []history.Entry
		for _, e := range entries {
			if e.Seq > seedLast && e.Kind == "done" {
				dones = append(dones, e)
			}
		}
		if len(dones) != 2 {
			t.Fatalf("want 2 new done entries after seq %d, got %d", seedLast, len(dones))
		}
		for i, d := range dones {
			chain := doubleInstanceSameSessionResumeChain(t, bySeq, d)
			passes := false
			for cur := d; ; {
				if cur.Seq == seedLast {
					passes = true
					break
				}
				p, ok := bySeq[history.ParentOf(cur)]
				if !ok {
					break
				}
				cur = p
			}
			if !passes {
				t.Errorf("done #%d (seq %d) does not descend from the resumed seq %d: %v", i+1, d.Seq, seedLast, chain)
			}
			in, as := 0, 0
			for _, k := range chain {
				switch k {
				case "input":
					in++
				case "assistant":
					as++
				}
			}
			if (in != 2 && in != 3) || in != as {
				t.Errorf("done #%d (seq %d) ancestry has %d inputs and %d assistants: %v", i+1, d.Seq, in, as, chain)
			}
		}
	})

	t.Run("SecondInstanceWarns", func(t *testing.T) {
		if !doubleInstanceSameSessionResumeWarning.MatchString(bScreen) {
			t.Errorf("second instance never mentions the concurrent writer:\n%s", bScreen)
		}
	})
}
