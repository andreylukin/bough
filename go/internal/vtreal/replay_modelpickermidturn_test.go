package vtreal

// /model picked while a turn is still streaming: the running turn must
// finish on (and be recorded as) the model it started on, the next turn
// must run on and record the new one, and the status bar's model chip
// must move once, not flicker back.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// modelPickerMidTurnTape is one turn whose single reply was written by
// "old-model" and is long enough, streamed with a delay, to still be
// running while the picker is driven.
func modelPickerMidTurnTape(t *testing.T) string {
	t.Helper()
	words := make([]string, 60)
	for i := range words {
		words[i] = fmt.Sprintf("w%02d", i)
	}
	reply := "```stop\n" + strings.Join(words, " ") + " finished\n```"
	tape := fmt.Sprintf(`{"seq":1,"kind":"meta","data":{"cwd":"/tmp"}}
{"seq":2,"kind":"input","data":{"text":"stream slowly"}}
{"seq":3,"kind":"assistant","data":{"text":%q,"model":"old-model","provider":"replay"}}
{"seq":4,"kind":"done","data":{"text":""}}
`, reply)
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, []byte(tape), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// modelPickerMidTurnAssistants is every top-level assistant entry this
// run wrote, oldest first.
func (a *app) modelPickerMidTurnAssistants() []history.Entry {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []history.Entry
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			if e.Kind == "assistant" {
				out = append(out, e)
			}
		}
	}
	return out
}

func (a *app) modelPickerMidTurnStatus() string {
	ls := a.lines()
	for i := len(ls) - 1; i >= 0; i-- {
		if strings.HasSuffix(ls[i], "? keys") {
			return ls[i]
		}
	}
	return ""
}

func TestModelPickerMidTurn(t *testing.T) {
	t.Parallel()
	tape := modelPickerMidTurnTape(t)
	cfg := strings.Replace(replayConfig(tape), "config: {file: "+fmt.Sprintf("%q", tape)+"}",
		"config: {file: "+fmt.Sprintf("%q", tape)+", delay_ms: 150}", 1)
	a := startCfg(t, 120, 30, cfg)
	a.check("boot")

	// Sample the chip for the whole run: it may go old-model → gone
	// once, never back.
	stop := make(chan struct{})
	var chips []bool
	sampled := make(chan struct{})
	go func() {
		defer close(sampled)
		for {
			select {
			case <-stop:
				return
			case <-time.After(30 * time.Millisecond):
			}
			st := a.modelPickerMidTurnStatus()
			if st == "" {
				continue
			}
			has := strings.Contains(st, "old-model")
			if len(chips) == 0 || chips[len(chips)-1] != has {
				chips = append(chips, has)
			}
		}
	}()

	a.typeText("stream slowly")
	a.key(uv.KeyEnter, 0)
	a.waitFor("w03")
	if a.doneCount() != 0 {
		t.Fatalf("turn finished before the picker opened; slow the tape")
	}
	a.typeText("/model")
	a.key(uv.KeyEnter, 0)
	a.waitFor("pick a model")
	a.typeText("llm-echo")
	a.waitFor("▸ llm-echo")
	a.key(uv.KeyEnter, 0)
	a.waitFor("model: llm-echo")
	if a.doneCount() != 0 {
		t.Errorf("the running turn ended (done/cancelled recorded) when /model swapped the llm row; it should finish on the old model")
	}
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("first turn never finished:\n%s", a.text())
	}
	a.waitFor("finished")
	a.check("after first turn")

	a.typeText("second turn")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(2, 30*time.Second) {
		t.Fatalf("second turn never finished:\n%s", a.text())
	}
	a.waitFor("echo: second turn")
	a.check("after second turn")
	a.settled()
	close(stop)
	<-sampled

	as := a.modelPickerMidTurnAssistants()
	if len(as) != 2 {
		t.Fatalf("want 2 assistant entries, got %d: %+v", len(as), as)
	}
	if m, p := as[0].Data["model"], as[0].Data["provider"]; m != "old-model" || p != "replay" {
		t.Errorf("first turn recorded model=%v provider=%v, want old-model/replay", m, p)
	}
	if m, p := as[1].Data["model"], as[1].Data["provider"]; m != nil || p != "echo" {
		t.Errorf("second turn recorded model=%v provider=%v, want no model/echo", m, p)
	}
	// Collapse: any run ending false after a true, with exactly one
	// true→false edge and no false→true after it.
	edges := 0
	for i := 1; i < len(chips); i++ {
		if chips[i-1] && !chips[i] {
			edges++
		}
		if !chips[i-1] && chips[i] && edges > 0 {
			t.Errorf("model chip came back after leaving: %v", chips)
		}
	}
	if edges != 1 || chips[len(chips)-1] {
		t.Errorf("model chip transitions %v: want old-model shown then gone exactly once\n%s", chips, a.text())
	}
}
