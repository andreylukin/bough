package vtreal

// The scratchpad on a real PTY: /scratch names the session's directory
// and lists notes.md, the prompt tells the model where it is, and a
// block result past the loop's cap spills to disk with a pointer the
// model can follow, while the composer stays put.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// scratchSession is the id of the session bough is writing under $HOME.
func (a *app) scratchSession() string {
	a.t.Helper()
	var paths []string
	a.waitUntil(func(string) bool {
		paths, _ = filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
		return len(paths) == 1
	}, "one history file")
	return strings.TrimSuffix(filepath.Base(paths[0]), ".jsonl")
}

// scratchEntries is the text of every entry of kind in this run's history.
func (a *app) scratchEntries(kind string) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []string
	for _, p := range paths {
		es, _ := history.Read(p)
		for _, e := range es {
			if e.Kind == kind {
				s, _ := e.Data["text"].(string)
				out = append(out, s)
			}
		}
	}
	return out
}

func TestScratchCommandShowsDirAndNotes(t *testing.T) {
	t.Parallel()
	a := start(t, 240, 30)
	dir := filepath.Join(a.home, ".bough", "scratch", a.scratchSession())

	a.typeText("/scratch")
	a.key(uv.KeyEnter, 0)
	a.waitFor("scratchpad: " + dir)
	a.waitFor("(empty)")
	a.check("/scratch empty")

	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "notes.md"), []byte("- 10:00 found it\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	a.typeText("/scratch")
	a.key(uv.KeyEnter, 0)
	a.waitFor("files: notes.md (17 B)")
	a.check("/scratch with notes")
}

func TestScratchPromptNamesDir(t *testing.T) {
	t.Parallel()
	a := start(t, 120, 30)
	dir := filepath.Join(a.home, ".bough", "scratch", a.scratchSession())
	a.typeText("SYSTEM!")
	a.key(uv.KeyEnter, 0)
	var prompt string
	a.waitUntil(func(string) bool {
		for _, s := range a.scratchEntries("assistant") {
			if strings.Contains(s, "Scratchpad — your own directory") {
				prompt = s
				return true
			}
		}
		return false
	}, "the echoed system prompt in history")
	for _, want := range []string{"at " + dir + " (also $BOUGH_SCRATCH in tools.bash)", "tools.scratch.note(text)"} {
		if !strings.Contains(prompt, want) {
			t.Errorf("prompt lacks %q:\n%s\nscreen:\n%s", want, prompt, a.text())
		}
	}
}

func TestScratchResultSpills(t *testing.T) {
	t.Parallel()
	var big strings.Builder
	for i := range 7000 {
		fmt.Fprintf(&big, "log line %05d\n", i) // 15 bytes each: 105 kB, past the loop's 64 kB cap
	}
	code := "console.log(tools.bash(\"cat big.log\"))\n"
	entries := []map[string]any{
		{"kind": "meta", "data": map[string]any{"cwd": "/tmp/demo"}},
		{"kind": "input", "data": map[string]any{"text": "dump the log"}},
		{"kind": "assistant", "data": map[string]any{"text": "```js\n" + code + "```"}},
		{"kind": "code", "data": map[string]any{"text": code}},
		{"kind": "result", "data": map[string]any{"code": code, "text": big.String()}},
		{"kind": "assistant", "data": map[string]any{"text": "```stop\nThe log is long.\n```"}},
		{"kind": "done", "data": map[string]any{"text": ""}},
	}
	var tape strings.Builder
	for i, e := range entries {
		e["seq"], e["at"] = i+1, "2026-09-10T10:00:00Z"
		b, _ := json.Marshal(e)
		tape.Write(append(b, '\n'))
	}
	path := filepath.Join(t.TempDir(), "scratch-spill.jsonl")
	if err := os.WriteFile(path, []byte(tape.String()), 0o644); err != nil {
		t.Fatal(err)
	}

	a := startCfg(t, 120, 30, replayConfig(path))
	a.typeText("dump the log")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("The log is long.")
	a.check("after spill")

	spills, _ := filepath.Glob(filepath.Join(a.home, ".bough", "spill", "result-*.log"))
	if len(spills) != 1 {
		t.Fatalf("want one spill file, got %v:\n%s", spills, a.text())
	}
	if b, _ := os.ReadFile(spills[0]); string(b) != big.String() {
		t.Errorf("spill file holds %d bytes, want the whole %d-byte result:\n%s", len(b), big.Len(), a.text())
	}
	digest := fmt.Sprintf("[full output saved to %s — 7000 lines; use tools.view or grep it]", spills[0])
	results := a.scratchEntries("result")
	if len(results) != 1 || !strings.HasSuffix(results[0], digest) || !strings.Contains(results[0], "bytes cut] …") {
		t.Errorf("result entry lacks the cut and the digest %q (got %d results):\n%s", digest, len(results), a.text())
	}
}
