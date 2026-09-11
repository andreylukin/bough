package vtreal

// The "@" file picker (plugins/ui/atfiles.go) on a real PTY: rows over
// the project's files, arrow navigation, tab/enter completion into the
// draft, esc, fuzzy filtering, and the completed path reaching the
// model as an expanded attachment (the run's own history "input").

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// atPickerFiles are written into the session cwd ($HOME) after boot
// (the picker reads the tree when it opens); with bough.yml they make
// atPickerAll.
var atPickerFiles = map[string]string{
	"README.md":     "readme\n",
	"docs/guide.md": "guide\n",
	"src/main.go":   "package main // atpicker-marker\n",
	"src/util.go":   "package main\n",
}

var atPickerAll = []string{"README.md", "bough.yml", "docs/guide.md", "src/main.go", "src/util.go"}

func atPickerStart(t *testing.T) *app {
	t.Helper()
	a := start(t, 100, 30)
	for name, body := range atPickerFiles {
		p := filepath.Join(a.home, name)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return a
}

// atPickerRows returns the picker rows off the screen, "  @name" or
// "> @name" (selected), skipping the composer row itself.
func atPickerRows(screen string) (names []string, sel string) {
	ls := strings.Split(screen, "\n")
	c := composerRow(ls)
	for i, l := range ls {
		if i == c {
			continue
		}
		switch {
		case strings.HasPrefix(l, "> @"):
			sel = strings.Fields(l[3:])[0]
			names = append(names, sel)
		case strings.HasPrefix(l, "  @") && len(strings.Fields(l[3:])) > 0:
			names = append(names, strings.Fields(l[3:])[0])
		}
	}
	return names, sel
}

func atPickerComposer(screen string) string {
	ls := strings.Split(screen, "\n")
	if c := composerRow(ls); c >= 0 {
		return strings.TrimRight(ls[c], " ")
	}
	return ""
}

// atPickerWant waits until the picker shows exactly names with sel
// selected (nil names: the picker is closed).
func (a *app) atPickerWant(where string, names []string, sel string) {
	a.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		s := a.settled()
		got, gotSel := atPickerRows(s)
		if strings.Join(got, ",") == strings.Join(names, ",") && gotSel == sel {
			return
		}
		if time.Now().After(deadline) {
			a.t.Fatalf("%s: picker rows %q (selected %q), want %q (selected %q):\n%s", where, got, gotSel, names, sel, s)
		}
	}
}

func (a *app) atPickerDraft(where, want string) {
	a.t.Helper()
	a.waitUntil(func(s string) bool { return atPickerComposer(s) == want }, where+": composer "+want)
}

// atPickerInput waits for the first "input" entry in this run's history.
func atPickerInput(t *testing.T, a *app) history.Entry {
	t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
		for _, p := range paths {
			entries, _ := history.Read(p)
			for _, e := range entries {
				if e.Kind == "input" {
					return e
				}
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("no input entry in history:\n%s", a.text())
	return history.Entry{}
}

func TestAtPickerRows(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("@")
	a.atPickerWant("@ opened", atPickerAll, "README.md")
	a.check("picker open")
}

func TestAtPickerArrowNavigation(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("@")
	a.atPickerWant("@ opened", atPickerAll, "README.md")
	a.key(uv.KeyDown, 0)
	a.key(uv.KeyDown, 0)
	a.atPickerWant("down x2", atPickerAll, "docs/guide.md")
	a.key(uv.KeyUp, 0)
	a.atPickerWant("up", atPickerAll, "bough.yml")
	a.key(uv.KeyUp, 0)
	a.key(uv.KeyUp, 0)
	a.atPickerWant("up past the top wraps", atPickerAll, "src/util.go")
	a.key(uv.KeyTab, 0)
	a.atPickerDraft("tab after arrows", "> @src/util.go")
	a.atPickerWant("closed after tab", nil, "")
}

func TestAtPickerFuzzyFilter(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("@util")
	a.atPickerWant("substring", []string{"src/util.go"}, "src/util.go")
	for range 4 {
		a.key(uv.KeyBackspace, 0)
	}
	a.typeText("smn") // s…m…n: only a subsequence of src/main.go
	a.atPickerWant("subsequence", []string{"src/main.go"}, "src/main.go")
	for range 3 {
		a.key(uv.KeyBackspace, 0)
	}
	a.typeText("src")
	a.atPickerWant("prefix", []string{"src/main.go", "src/util.go"}, "src/main.go")
	a.typeText("zzz")
	a.atPickerWant("no match hides the picker", nil, "")
}

func TestAtPickerTabCompletes(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("look at @mai")
	a.atPickerWant("filtered", []string{"src/main.go"}, "src/main.go")
	a.key(uv.KeyTab, 0)
	a.atPickerDraft("tab", "> look at @src/main.go")
	a.atPickerWant("closed after tab", nil, "")
}

func TestAtPickerEnterCompletesWithoutSubmitting(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("@gui")
	a.atPickerWant("filtered", []string{"docs/guide.md"}, "docs/guide.md")
	a.key(uv.KeyEnter, 0)
	a.atPickerDraft("enter", "> @docs/guide.md")
	if s := a.settled(); strings.Contains(s, "❯") || strings.Contains(s, "echo:") {
		t.Fatalf("enter in the picker submitted the draft:\n%s", s)
	}
	a.atPickerWant("closed after enter", nil, "")
}

func TestAtPickerEscCancels(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("@")
	a.atPickerWant("@ opened", atPickerAll, "README.md")
	a.key(uv.KeyEscape, 0)
	a.atPickerWant("esc", nil, "")
	a.atPickerDraft("esc keeps the draft", "> @")
	a.typeText("util") // a changed draft reopens it, filtered
	a.atPickerWant("reopened", []string{"src/util.go"}, "src/util.go")
}

func TestAtPickerModelReceivesPath(t *testing.T) {
	t.Parallel()
	a := atPickerStart(t)
	a.typeText("read @mai")
	a.atPickerWant("filtered", []string{"src/main.go"}, "src/main.go")
	a.key(uv.KeyTab, 0)
	a.typeText("now")
	a.atPickerDraft("tab + more text", "> read @src/main.go now")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: read @src/main.go now")
	in := atPickerInput(t, a)
	if got, _ := in.Data["typed"].(string); got != "read @src/main.go now" {
		t.Errorf("input typed = %q, want the completed draft:\n%s", got, a.text())
	}
	text, _ := in.Data["text"].(string)
	if !strings.Contains(text, "[file: src/main.go]\npackage main // atpicker-marker") {
		t.Errorf("model input lacks the expanded file:\n%q\nscreen:\n%s", text, a.text())
	}
	a.check("after turn")
}
