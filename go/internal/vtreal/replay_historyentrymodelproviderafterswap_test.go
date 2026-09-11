package vtreal

// Assistant-entry provenance across a /model swap and a fork: turn 1
// is served by the replay row (tape model "model-a", provider
// "replay"), /model swaps the llm row to llm-echo (provider "echo",
// no model name), turn 2 runs on it, /tree forks at turn 1 and a turn
// runs on the fork. Every assistant entry must name what served it,
// the fork must keep turn 1's entry as written, and a resume of the
// fork must neither rewrite the record nor show a model chip the
// running row does not serve.

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// historyEntryModelProviderAfterSwapTape is one turn answered by
// "model-a".
func historyEntryModelProviderAfterSwapTape(t *testing.T) string {
	t.Helper()
	tape := `{"seq":1,"kind":"meta","data":{"cwd":"/tmp"}}
{"seq":2,"kind":"input","data":{"text":"turn one"}}
{"seq":3,"kind":"assistant","data":{"text":"` + "```stop\\nHEMP-ONE\\n```" + `","model":"model-a","provider":"replay"}}
{"seq":4,"kind":"done","data":{"text":""}}
`
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, []byte(tape), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// historyEntryModelProviderAfterSwapHome is a fresh $HOME with a
// models.json stub so nothing refreshes from the network.
func historyEntryModelProviderAfterSwapHome(t *testing.T) string {
	t.Helper()
	home, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(home, ".bough", "history"), 0o755); err != nil {
		t.Fatal(err)
	}
	stub := `{"v":2,"c":{"hemp":{"none":{}}}}`
	if err := os.WriteFile(filepath.Join(home, ".bough", "models.json"), []byte(stub), 0o644); err != nil {
		t.Fatal(err)
	}
	return home
}

// historyEntryModelProviderAfterSwapProv is "model/provider" for every
// assistant entry in path, oldest first ("-" for a missing key).
func historyEntryModelProviderAfterSwapProv(t *testing.T, path string) []string {
	t.Helper()
	entries, err := history.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	var out []string
	for _, e := range entries {
		if e.Kind != "assistant" {
			continue
		}
		m, ok := e.Data["model"].(string)
		if !ok {
			m = "-"
		}
		p, ok := e.Data["provider"].(string)
		if !ok {
			p = "-"
		}
		out = append(out, m+"/"+p)
	}
	return out
}

// historyEntryModelProviderAfterSwapFiles is the original session file
// and the fork file for seq ("" when absent).
func historyEntryModelProviderAfterSwapFiles(home, seq string) (orig, fork string) {
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	for _, p := range paths {
		if strings.HasSuffix(p, "-f"+seq+".jsonl") {
			fork = p
		} else if !strings.Contains(filepath.Base(p), "-f") {
			orig = p
		}
	}
	return orig, fork
}

func (a *app) historyEntryModelProviderAfterSwapStatus() string {
	ls := a.lines()
	for i := len(ls) - 1; i >= 0; i-- {
		if strings.Contains(ls[i], "? keys") {
			return ls[i]
		}
	}
	return ""
}

func TestHistoryEntryModelProviderAfterSwap(t *testing.T) {
	t.Parallel()
	tape := historyEntryModelProviderAfterSwapTape(t)
	home := historyEntryModelProviderAfterSwapHome(t)
	a := undoStart(t, home, 120, 30, replayConfig(tape))

	t.Run("TurnOnModelA", func(t *testing.T) {
		followUpTurn(a, "turn one", 1)
		a.waitFor("HEMP-ONE")
		a.waitUntil(func(string) bool {
			return strings.Contains(a.historyEntryModelProviderAfterSwapStatus(), "model-a")
		}, "model-a chip after turn 1")
		a.check("turn 1")
	})
	if t.Failed() {
		return
	}

	t.Run("SwapAndTurnOnB", func(t *testing.T) {
		a.typeText("/model llm-echo")
		a.key(uv.KeyEnter, 0)
		a.waitFor("model: llm-echo")
		followUpTurn(a, "turn two", 2)
		a.waitFor("echo: turn two")
		a.settled()
		if st := a.historyEntryModelProviderAfterSwapStatus(); strings.Contains(st, "model-a") {
			t.Errorf("status bar still names model-a after the swap: %q", st)
		}
		a.check("turn 2")
	})
	if t.Failed() {
		return
	}

	seq := ""
	for _, e := range followUpNewest(a) {
		if e.Kind == "input" {
			seq = strconv.FormatInt(e.Seq, 10)
			break
		}
	}
	orig, _ := historyEntryModelProviderAfterSwapFiles(home, seq)
	if got := historyEntryModelProviderAfterSwapProv(t, orig); strings.Join(got, " ") != "model-a/replay -/echo" {
		t.Errorf("original session provenance = %q, want model-a/replay then -/echo", got)
	}

	t.Run("ForkAndTurn", func(t *testing.T) {
		a.typeText("/tree " + seq)
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool {
			_, f := historyEntryModelProviderAfterSwapFiles(home, seq)
			return f != ""
		}, "the fork file")
		// The fork keeps the llm row in effect (echo): nothing in the
		// session restores a model.
		a.typeText("fork turn")
		a.key(uv.KeyEnter, 0)
		followUpWaitDone(a, 2)
		a.waitFor("echo: fork turn")
		a.check("fork turn")
		_, fork := historyEntryModelProviderAfterSwapFiles(home, seq)
		if got := historyEntryModelProviderAfterSwapProv(t, fork); strings.Join(got, " ") != "model-a/replay -/echo" {
			t.Errorf("fork provenance = %q, want turn 1's model-a/replay then the fork turn's -/echo", got)
		}
	})
	if t.Failed() {
		return
	}
	rewindAfterResumeAndForkQuit(t, a)

	_, fork := historyEntryModelProviderAfterSwapFiles(home, seq)
	before := historyEntryModelProviderAfterSwapProv(t, fork)

	// Resume on the swapped config: echo serves and names no model, so
	// no model chip may show (least of all model-a from the record).
	t.Run("ResumeForkOnEcho", func(t *testing.T) {
		yml := strings.Replace(replayConfig(tape), "- id: llm\n  plugin: replay\n", "- id: llm\n  plugin: llm-echo\n", 1)
		const row = "- id: history\n  plugin: history\n"
		yml = strings.Replace(yml, row, row+"  config: {file: \""+fork+"\"}\n", 1)
		b := undoStart(t, home, 120, 30, yml)
		b.waitFor("echo: fork turn")
		b.settled()
		if st := b.historyEntryModelProviderAfterSwapStatus(); strings.Contains(st, "model-a") {
			t.Errorf("resumed on echo, status bar names model-a: %q", st)
		}
		b.check("resumed")
		rewindAfterResumeAndForkQuit(t, b)
		if got := historyEntryModelProviderAfterSwapProv(t, fork); strings.Join(got, " ") != strings.Join(before, " ") {
			t.Errorf("resume rewrote provenance: %q -> %q", before, got)
		}
	})
}
