package vtreal

// Surface "scratch-and-artifact-with-session-rename-resume": run one
// writes scratch files and a note, publishes an artifact on an offline
// (loopback, ephemeral) web port, and is renamed by the session-title
// row to a unicode title (the tape answers the namer's Complete call —
// there is no /rename command, the title entry is the only name a
// session has). It quits. Run two boots fresh in the same $HOME on the
// same port, resumes run one through the /sessions picker, and must
// see: the title in the picker and in the tab title (OSC 0/2), the
// same scratch dir behind tools.scratch.dir() and $BOUGH_SCRATCH with
// the files and note still there, and the artifact still served.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const scratchAndArtifactWithSessionRenameResumeTitle = "Überprüfe 日本語 café ✨ Ω"

const scratchAndArtifactWithSessionRenameResumeProgram = `root = Card([head])
head = CardHeader("Renamed page", "resume")`

var scratchAndArtifactWithSessionRenameResumeURLRe = regexp.MustCompile(`http://127\.0\.0\.1:\d+/artifacts/([^/\s"]+)/renamed\b`)

// scratchAndArtifactWithSessionRenameResumeConfig is the real runtime
// (codemode, tools, scratchpad, artifacts on a web row at addr) with
// the replay llm; titled turns the session-title row on.
func scratchAndArtifactWithSessionRenameResumeConfig(tape, addr string, titled bool) string {
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
- id: web
  plugin: web
  config: {addr: %q}
- id: artifacts
  plugin: artifacts
  config: {open: false}
- id: session-title
  plugin: session-title
  disabled: %v
- id: activity
  plugin: activity
  disabled: true
`, tape, addr, !titled)
}

func scratchAndArtifactWithSessionRenameResumeTape(t *testing.T, name string, replies ...string) string {
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

// scratchAndArtifactWithSessionRenameResumeProbe reports the pad dir,
// $BOUGH_SCRATCH, the notes and the listing, after body.
func scratchAndArtifactWithSessionRenameResumeProbe(tag, body string) string {
	return "```js\n// phase:" + tag + "\n" + body + `
console.log("DIR=" + tools.scratch.dir())
console.log("ENV=" + tools.bash("printf '%s' \"$BOUGH_SCRATCH\""))
console.log("NOTES=" + tools.scratch.notes().split("\n").join(" "))
console.log("LIST=" + tools.scratch.list().split("\n").join(" "))
` + "```"
}

// scratchAndArtifactWithSessionRenameResumeResult waits for the result
// entry of the block tagged tag in any session file under home.
func scratchAndArtifactWithSessionRenameResumeResult(a *app, tag string) string {
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

func scratchAndArtifactWithSessionRenameResumeLine(out, key string) string {
	for _, l := range strings.Split(out, "\n") {
		if v, ok := strings.CutPrefix(l, key+"="); ok {
			return strings.TrimSpace(v)
		}
	}
	return ""
}

// scratchAndArtifactWithSessionRenameResumeTab polls the recorded tab
// title until pred holds.
func scratchAndArtifactWithSessionRenameResumeTab(a *app, what string, pred func(string) bool) {
	a.t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		if pred(a.term.Snapshot().Title) {
			return
		}
		time.Sleep(25 * time.Millisecond)
	}
	a.t.Errorf("tab title %q, want %s:\n%s", a.term.Snapshot().Title, what, a.text())
}

func TestScratchAndArtifactWithSessionRenameResume(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	addr := webModeOfflinePort(t)
	title := scratchAndArtifactWithSessionRenameResumeTitle
	mark := fmt.Sprintf("MARK-%d", time.Now().UnixNano())

	// Run one: files + note + artifact, then the namer's reply.
	write := fmt.Sprintf(`tools.scratch.note(%q)
tools.bash("printf alpha > \"$BOUGH_SCRATCH/alpha.txt\"")
var betaPath = tools.scratch.file("beta.txt")
tools.bash("printf beta > '" + betaPath + "'")
console.log("URL=" + tools.artifact("renamed", %q))`, mark, scratchAndArtifactWithSessionRenameResumeProgram)
	tape1 := scratchAndArtifactWithSessionRenameResumeTape(t, "first.jsonl",
		scratchAndArtifactWithSessionRenameResumeProbe("first", write),
		"```stop\nsaved and published.\n```",
		title)
	first := crashResumeIntegrityStart(t, home, scratchAndArtifactWithSessionRenameResumeConfig(tape1, addr, true))
	first.typeText("keep notes and publish a page")
	first.key(uv.KeyEnter, 0)
	out1 := scratchAndArtifactWithSessionRenameResumeResult(first, "first")
	first.waitFor("saved and published.")
	hist1 := crashResumeIntegritySession(t, home)
	id1 := strings.TrimSuffix(filepath.Base(hist1), ".jsonl")
	dir1 := filepath.Join(home, ".bough", "scratch", id1)
	if got := scratchAndArtifactWithSessionRenameResumeLine(out1, "DIR"); got != dir1 {
		t.Fatalf("run one pad dir = %q, want %q\n%s", got, dir1, out1)
	}
	if got := scratchAndArtifactWithSessionRenameResumeLine(out1, "ENV"); got != dir1 {
		t.Fatalf("run one $BOUGH_SCRATCH = %q, want %q\n%s", got, dir1, out1)
	}
	m := scratchAndArtifactWithSessionRenameResumeURLRe.FindStringSubmatch(out1)
	if m == nil {
		t.Fatalf("no artifact URL in run one's result:\n%s", out1)
	}
	url := m[0]
	if !strings.HasPrefix(url, "http://"+addr+"/") {
		t.Fatalf("artifact URL %s is not on the test's port %s", url, addr)
	}
	if m[1] != id1 {
		t.Errorf("artifact filed under session %q, want %q", m[1], id1)
	}
	for _, f := range []string{"alpha.txt", "beta.txt"} {
		if _, err := os.Stat(filepath.Join(dir1, f)); err != nil {
			t.Fatalf("run one did not write %s into the pad: %v\n%s", f, err, out1)
		}
	}
	// The rename lands as a title entry and on the tab.
	first.waitUntil(func(string) bool {
		es, _ := history.Read(hist1)
		for _, e := range es {
			if s, _ := e.Data["text"].(string); e.Kind == "title" && s == title {
				return true
			}
		}
		return false
	}, "the unicode title entry")
	scratchAndArtifactWithSessionRenameResumeTab(first, "✓ "+title, func(s string) bool { return s == "✓ "+title })
	if code, body, err := webModeOfflineGet(url); err != nil || code != 200 || !strings.Contains(body, "Renamed page") {
		t.Fatalf("run one does not serve its artifact: %d %v %.200s", code, err, body)
	}
	webModeOfflineQuit(t, first) // the port is free again for run two

	// Run two: fresh boot on the same port, resume through the picker.
	tape2 := scratchAndArtifactWithSessionRenameResumeTape(t, "second.jsonl",
		scratchAndArtifactWithSessionRenameResumeProbe("resumed", ""),
		"```stop\nresumed ok.\n```")
	second := crashResumeIntegrityStart(t, home, scratchAndArtifactWithSessionRenameResumeConfig(tape2, addr, false))

	t.Run("PickerShowsTitle", func(t *testing.T) {
		second.t = t
		newSessionOpenPicker(second)
		second.waitFor(title)
		selected := func() bool {
			for _, l := range second.lines() {
				if strings.Contains(l, "▸ ") && strings.Contains(l, title) {
					return true
				}
			}
			return false
		}
		for i := 0; i < 5 && !selected(); i++ {
			second.key(uv.KeyDown, 0)
			second.settled()
		}
		if !selected() {
			t.Fatalf("never selected the titled row:\n%s", second.text())
		}
		second.key(uv.KeyEnter, 0)
		second.waitFor("resumed " + id1)
	})

	t.Run("TabTitleAfterResume", func(t *testing.T) {
		second.t = t
		scratchAndArtifactWithSessionRenameResumeTab(second, "ending in "+title, func(s string) bool {
			return strings.HasSuffix(s, title)
		})
	})

	t.Run("ScratchSameDir", func(t *testing.T) {
		second.t = t
		second.typeText("what is in the pad")
		second.key(uv.KeyEnter, 0)
		out := scratchAndArtifactWithSessionRenameResumeResult(second, "resumed")
		second.waitFor("resumed ok.")
		if got := scratchAndArtifactWithSessionRenameResumeLine(out, "DIR"); got != dir1 {
			t.Errorf("resumed pad dir = %q, want run one's %q\n%s", got, dir1, out)
		}
		if got := scratchAndArtifactWithSessionRenameResumeLine(out, "ENV"); got != dir1 {
			t.Errorf("resumed $BOUGH_SCRATCH = %q, want run one's %q\n%s", got, dir1, out)
		}
		if !strings.Contains(out, mark) {
			t.Errorf("resumed notes lack %s:\n%s", mark, out)
		}
		list := scratchAndArtifactWithSessionRenameResumeLine(out, "LIST")
		for _, f := range []string{"alpha.txt", "beta.txt"} {
			if !strings.Contains(list, f) {
				t.Errorf("resumed listing lacks %s: %q", f, list)
			}
		}
		scratchAndArtifactWithSessionRenameResumeTab(second, "✓ "+title+" after a turn", func(s string) bool { return s == "✓ "+title })
	})

	t.Run("ArtifactStillServed", func(t *testing.T) {
		second.t = t
		code, body, err := webModeOfflineGet(url)
		if err != nil || code != 200 || !strings.Contains(body, "Renamed page") {
			t.Errorf("run two does not serve run one's artifact %s: %d %v %.200s", url, code, err, body)
		}
		code, body, err = webModeOfflineGet(url + ".ui")
		if err != nil || code != 200 || strings.TrimSpace(body) != scratchAndArtifactWithSessionRenameResumeProgram {
			t.Errorf("program after resume: %d %v %q", code, err, body)
		}
	})
}
