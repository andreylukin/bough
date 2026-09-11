package vtreal

// The scratchpad across a session's life on a real PTY: a first run
// writes a unique marker into notes.md, quits; a second run resumes
// the same session with -r and must find the same directory, the same
// notes and $BOUGH_SCRATCH pointing there; /new then swaps to a fresh
// session whose directory is new, empty of the old notes, and whose
// own note never lands in the old one.
//
// Determinism: the llm is a replay tape, but codemode, tools and the
// scratchpad are the real rows, so the taped blocks really write and
// read disk. Every block carries a phase tag in its code; assertions
// read the result entries by tag and stat the paths.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// scratchDirLifecycleOnResumeConfig is replayConfig with the real
// runtime: codemode, tools and the scratchpad row.
func scratchDirLifecycleOnResumeConfig(tape string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: codemode
  plugin: codemode
- id: tools
  plugin: tools-basic
- id: scratchpad
  plugin: scratchpad
- id: commands
  plugin: commands
- id: history
  plugin: history
- id: loop
  plugin: loop
- id: ui
  plugin: ui
- id: session-title
  plugin: session-title
  disabled: true
- id: auto-memory
  plugin: auto-memory
  disabled: true
- id: memory-tier
  plugin: memory-tier
  disabled: true
- id: activity
  plugin: activity
  disabled: true
- id: attention
  plugin: attention
  disabled: true
`, tape)
}

// scratchDirLifecycleOnResumeTape writes a tape of assistant replies.
func scratchDirLifecycleOnResumeTape(t *testing.T, name string, replies ...string) string {
	t.Helper()
	var b strings.Builder
	for i, r := range replies {
		j, _ := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z",
			"kind": "assistant", "data": map[string]any{"text": r}})
		b.Write(append(j, '\n'))
	}
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// scratchDirLifecycleOnResumeProbe is a block that reports the pad's
// dir, the shell's $BOUGH_SCRATCH and the notes, after an optional note.
func scratchDirLifecycleOnResumeProbe(tag, note string) string {
	code := "// phase:" + tag + "\n"
	if note != "" {
		code += fmt.Sprintf("tools.scratch.note(%q)\n", note)
	}
	code += `console.log("DIR=" + tools.scratch.dir())
console.log("ENV=" + tools.bash("printf '%s' \"$BOUGH_SCRATCH\""))
console.log("NOTES=" + tools.scratch.notes())
`
	return "```js\n" + code + "```"
}

// scratchDirLifecycleOnResumeResult waits for the result entry of the
// block tagged tag, in any session file under home.
func scratchDirLifecycleOnResumeResult(a *app, tag string) string {
	a.t.Helper()
	var out string
	a.waitUntil(func(string) bool {
		paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
		for _, p := range paths {
			es, _ := history.Read(p)
			for _, e := range es {
				code, _ := e.Data["code"].(string)
				if e.Kind == "result" && strings.Contains(code, "// phase:"+tag+"\n") {
					out, _ = e.Data["text"].(string)
					return true
				}
			}
		}
		return false
	}, "the result of block "+tag)
	return out
}

// scratchDirLifecycleOnResumeLine is the value of the KEY= line.
func scratchDirLifecycleOnResumeLine(out, key string) string {
	for _, l := range strings.Split(out, "\n") {
		if v, ok := strings.CutPrefix(l, key+"="); ok {
			return strings.TrimSpace(v)
		}
	}
	return ""
}

func scratchDirLifecycleOnResumeSessions(t *testing.T, home string) []string {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	var ids []string
	for _, p := range paths {
		ids = append(ids, strings.TrimSuffix(filepath.Base(p), ".jsonl"))
	}
	slices.Sort(ids)
	return ids
}

func TestScratchDirLifecycleOnResume(t *testing.T) {
	t.Parallel()
	markA := fmt.Sprintf("MARK-A-%d", time.Now().UnixNano())
	markC := fmt.Sprintf("MARK-C-%d", time.Now().UnixNano())
	home := t.TempDir()

	// Run one: note the marker, quit.
	tape1 := scratchDirLifecycleOnResumeTape(t, "first.jsonl",
		scratchDirLifecycleOnResumeProbe("first", markA), "```stop\nnoted first.\n```")
	first := crashResumeIntegrityStart(t, home, scratchDirLifecycleOnResumeConfig(tape1))
	first.typeText("note it")
	first.key(uv.KeyEnter, 0)
	out1 := scratchDirLifecycleOnResumeResult(first, "first")
	first.waitFor("noted first.")
	ids := scratchDirLifecycleOnResumeSessions(t, home)
	if len(ids) != 1 {
		t.Fatalf("want one session after run one, got %v", ids)
	}
	id1 := ids[0]
	dir1 := filepath.Join(home, ".bough", "scratch", id1)
	if got := scratchDirLifecycleOnResumeLine(out1, "DIR"); got != dir1 {
		t.Fatalf("run one pad dir = %q, want %q\n%s", got, dir1, out1)
	}
	if got := scratchDirLifecycleOnResumeLine(out1, "ENV"); got != dir1 {
		t.Errorf("run one $BOUGH_SCRATCH = %q, want %q", got, dir1)
	}
	notes1, err := os.ReadFile(filepath.Join(dir1, "notes.md"))
	if err != nil || !strings.Contains(string(notes1), markA) {
		t.Fatalf("run one notes.md (%v) lacks %s:\n%s", err, markA, notes1)
	}
	first.key('c', uv.ModCtrl)
	first.key('c', uv.ModCtrl)

	// Run two: resume, probe; then /new and note in the fresh session.
	tape2 := scratchDirLifecycleOnResumeTape(t, "second.jsonl",
		scratchDirLifecycleOnResumeProbe("resumed", ""), "```stop\nresumed ok.\n```",
		scratchDirLifecycleOnResumeProbe("fresh", markC), "```stop\nfresh ok.\n```")
	second := crashResumeIntegrityStart(t, home, scratchDirLifecycleOnResumeConfig(tape2), "-r", id1)

	t.Run("ResumedSeesSameDirAndNotes", func(t *testing.T) {
		second.t = t
		second.typeText("what did I note")
		second.key(uv.KeyEnter, 0)
		out := scratchDirLifecycleOnResumeResult(second, "resumed")
		second.waitFor("resumed ok.")
		if got := scratchDirLifecycleOnResumeLine(out, "DIR"); got != dir1 {
			t.Errorf("resumed pad dir = %q, want run one's %q\n%s", got, dir1, out)
		}
		if got := scratchDirLifecycleOnResumeLine(out, "ENV"); got != dir1 {
			t.Errorf("resumed $BOUGH_SCRATCH = %q, want %q\n%s", got, dir1, out)
		}
		if !strings.Contains(out, markA) {
			t.Errorf("resumed notes lack %s:\n%s", markA, out)
		}
	})

	t.Run("NewGetsFreshDirNoLeak", func(t *testing.T) {
		second.t = t
		second.typeText("/new")
		second.waitFor("> /new")
		second.key(uv.KeyEnter, 0)
		second.settled()
		second.typeText("start fresh")
		second.key(uv.KeyEnter, 0)
		out := scratchDirLifecycleOnResumeResult(second, "fresh")
		second.waitFor("fresh ok.")
		ids := scratchDirLifecycleOnResumeSessions(t, home)
		if len(ids) != 2 || !slices.Contains(ids, id1) {
			t.Fatalf("want run one's session plus one new, got %v", ids)
		}
		id2 := ids[0]
		if id2 == id1 {
			id2 = ids[1]
		}
		dir2 := filepath.Join(home, ".bough", "scratch", id2)
		if got := scratchDirLifecycleOnResumeLine(out, "DIR"); got != dir2 {
			t.Errorf("new session pad dir = %q, want %q (old %q)\n%s", got, dir2, dir1, out)
		}
		if got := scratchDirLifecycleOnResumeLine(out, "ENV"); got != dir2 {
			t.Errorf("new session $BOUGH_SCRATCH = %q, want %q (old %q)\n%s", got, dir2, dir1, out)
		}
		if strings.Contains(out, markA) {
			t.Errorf("new session sees the old session's note %s:\n%s", markA, out)
		}
		if b, err := os.ReadFile(filepath.Join(dir2, "notes.md")); err != nil || !strings.Contains(string(b), markC) || strings.Contains(string(b), markA) {
			t.Errorf("new notes.md (%v) should hold only %s:\n%s", err, markC, b)
		}
		if b, _ := os.ReadFile(filepath.Join(dir1, "notes.md")); strings.Contains(string(b), markC) {
			t.Errorf("the new session's note leaked into the old notes.md:\n%s", b)
		}
		ents, _ := os.ReadDir(filepath.Join(home, ".bough", "scratch"))
		var names []string
		for _, e := range ents {
			names = append(names, e.Name())
		}
		slices.Sort(names)
		if want := []string{id1, id2}; !slices.Equal(names, slices.Sorted(slices.Values(want))) {
			t.Errorf("scratch dirs = %v, want exactly %v", names, want)
		}
	})
}
