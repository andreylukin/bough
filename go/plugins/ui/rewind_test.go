package ui

// The rewind menu: a list you walk with the arrows and pick from, the
// way Claude Code's Esc+Esc opens one. It used to print the turns into
// the transcript, which you could read but not use.

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// It opens on "(current)": the present, walking back from there.
func TestRewindOpensOnCurrent(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two", "three")
	d.press(keyEsc())
	d.press(keyEsc())
	if got, want := d.m.rw.pick, len(d.m.rw.rows)-1; got != want {
		t.Fatalf("cursor at %d, want %d ((current))", got, want)
	}
	if !strings.Contains(d.plain(), "❯ (current)") {
		t.Errorf("(current) should be the selected row:\n%s", d.plain())
	}
}

func TestRewindArrowsWalkTheTurns(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two", "three")
	d.press(keyEsc())
	d.press(keyEsc())

	d.press(keyUp())
	if !strings.Contains(d.plain(), "❯ three") {
		t.Errorf("up from (current) selects the newest turn:\n%s", d.plain())
	}
	d.press(keyUp())
	d.press(keyUp())
	if !strings.Contains(d.plain(), "❯ one") {
		t.Errorf("three ups reach the oldest turn:\n%s", d.plain())
	}
	d.press(keyUp()) // at the top: stays
	if !strings.Contains(d.plain(), "❯ one") {
		t.Errorf("up at the top should not wrap:\n%s", d.plain())
	}
	d.press(keyDown())
	if !strings.Contains(d.plain(), "❯ two") {
		t.Errorf("down walks forward again:\n%s", d.plain())
	}
}

// Picking a row goes back to BEFORE that prompt, so undoing "two"
// means the session ends at "one". Fork keeps the turn it is given, so
// that is a fork at "one" (seq 1), not at "two".
func TestRewindEnterGoesBackToBeforeThePrompt(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp()) // "two", whose own input entry is seq 3
	d.press(keyEnter())
	if d.m.rw.open {
		t.Error("enter should close the menu")
	}
	p := d.plain()
	if !strings.Contains(p, "/tree 1") {
		t.Errorf("before \"two\" is a fork at \"one\" (seq 1):\n%s", p)
	}
	if strings.Contains(p, "/tree 3") {
		t.Errorf("forking AT the picked turn would keep it:\n%s", p)
	}
}

// Before the first prompt there is no earlier turn: that point is a
// session with nothing in it.
func TestRewindToBeforeTheFirstPromptStartsFresh(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one", "two")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp())
	d.press(keyUp()) // "one", the oldest
	d.press(keyEnter())
	if p := d.plain(); !strings.Contains(p, "/new") {
		t.Errorf("before the first prompt is a fresh session:\n%s", p)
	}
}

// Enter on "(current)" changes nothing: it is the way out that is not
// a decision.
func TestRewindEnterOnCurrentDoesNothing(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyEnter())
	if d.m.rw.open {
		t.Error("the menu should close")
	}
	if p := d.plain(); strings.Contains(p, "/tree") {
		t.Errorf("(current) should not fork anything:\n%s", p)
	}
}

func TestRewindEscCancels(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "one")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyEsc())
	if d.m.rw.open {
		t.Error("esc should close the menu")
	}
	if p := d.plain(); strings.Contains(p, "/tree") {
		t.Errorf("cancelling must not fork:\n%s", p)
	}
}

// Each row says what its turn wrote, so you can see which points have
// code behind them — bough forks the CONVERSATION, and putting files
// back is /undo.
func TestRewindRowsShowWhatEachTurnWrote(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/s.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "wrote nothing"}},
		{Seq: 2, Kind: "done", Data: map[string]any{"files": []string{}}},
		{Seq: 3, Kind: "input", Data: map[string]any{"text": "wrote one"}},
		{Seq: 4, Kind: "done", Data: map[string]any{"files": []string{"only.go"}}},
		{Seq: 5, Kind: "input", Data: map[string]any{"text": "wrote several"}},
		{Seq: 6, Kind: "done", Data: map[string]any{"files": []string{"a.go", "b.go", "c.go"}}},
	}}
	cfg := cfgWith(t, nil, nil, h)
	cfg.cmds = reg(t, "tree", "new")
	d := newDrv(t, 100, 30, cfg)
	d.press(keyEsc())
	d.press(keyEsc())
	p := d.plain()
	for _, want := range []string{"no files written", "wrote only.go", "wrote 3 files"} {
		if !strings.Contains(p, want) {
			t.Errorf("the menu should say %q:\n%s", want, p)
		}
	}
}

// A background job's wake-up is recorded as an input, but nobody typed
// it, so it is not a point you rewind to.
func TestRewindSkipsBackgroundWakeups(t *testing.T) {
	t.Parallel()
	rows := rewindTurns([]history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "a real prompt"}},
		{Seq: 2, Kind: "input", Data: map[string]any{"text": "[background job] A command you started has finished"}},
	})
	if len(rows) != 1 || rows[0].text != "a real prompt" {
		t.Fatalf("only typed prompts are rewind points, got %+v", rows)
	}
}

// The prompt shown is what was TYPED: a skill's body is appended to
// the message that was sent, and it is not a menu row.
func TestRewindShowsTheTypedPrompt(t *testing.T) {
	t.Parallel()
	rows := rewindTurns([]history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{
			"text":  "please frobnicate\n\n[skill: frobnicate]\nlots of body",
			"typed": "please frobnicate",
		}},
	})
	if len(rows) != 1 || rows[0].text != "please frobnicate" {
		t.Fatalf("row should be the typed line, got %+v", rows)
	}
}

// Going back to before a turn is how you re-ask it, so the prompt you
// rewound past goes back in the composer — retyping it from the
// transcript is the whole of the work you just undid.
func TestRewindPrepopulatesTheComposer(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "add the parser", "fix the tests")
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp()) // "fix the tests"
	d.press(keyEnter())
	if got := d.m.input.Value(); got != "fix the tests" {
		t.Fatalf("composer = %q, want the prompt that was rewound past", got)
	}
	// And it is editable text, not a recall browse: typing appends.
	d.typeStr(" again")
	if got := d.m.input.Value(); got != "fix the tests again" {
		t.Errorf("the prepopulated draft should be editable, got %q", got)
	}
}

// The WHOLE prompt, not the one line the menu row showed.
func TestRewindPrepopulatesTheFullPrompt(t *testing.T) {
	t.Parallel()
	full := "first line of the ask\nsecond line\nthird line"
	h := fakeHist{path: "/tmp/s.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "earlier"}},
		{Seq: 2, Kind: "done", Data: map[string]any{}},
		{Seq: 3, Kind: "input", Data: map[string]any{"text": full}},
		{Seq: 4, Kind: "done", Data: map[string]any{}},
	}}
	cfg := cfgWith(t, nil, nil, h)
	cfg.cmds = reg(t, "tree", "new")
	d := newDrv(t, 100, 30, cfg)
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp())
	if !strings.Contains(d.plain(), "❯ first line of the ask") {
		t.Fatalf("the row shows one line:\n%s", d.plain())
	}
	d.press(keyEnter())
	if got := d.m.input.Value(); got != full {
		t.Errorf("the composer should get every line:\n got %q\nwant %q", got, full)
	}
}

// A skill's body is not part of what you typed, so it is not what comes
// back either.
func TestRewindPrepopulatesTheTypedPromptOnly(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/s.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "input", Data: map[string]any{"text": "earlier"}},
		{Seq: 2, Kind: "done", Data: map[string]any{}},
		{Seq: 3, Kind: "input", Data: map[string]any{
			"text":  "please frobnicate\n\n[skill: frobnicate]\nlots of body",
			"typed": "please frobnicate",
		}},
		{Seq: 4, Kind: "done", Data: map[string]any{}},
	}}
	cfg := cfgWith(t, nil, nil, h)
	cfg.cmds = reg(t, "tree", "new")
	d := newDrv(t, 100, 30, cfg)
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp())
	d.press(keyEnter())
	if got := d.m.input.Value(); got != "please frobnicate" {
		t.Errorf("composer = %q, want the typed line alone", got)
	}
}

// Cancelling, or picking "(current)", leaves the composer alone.
func TestRewindLeavesTheComposerAloneWhenNothingChanges(t *testing.T) {
	t.Parallel()
	for _, key := range []string{"esc", "enter"} { // esc cancels; enter on (current)
		d := rewindDrv(t, "one", "two")
		d.press(keyEsc())
		d.press(keyEsc())
		if key == "esc" {
			d.press(keyEsc())
		} else {
			d.press(keyEnter())
		}
		if got := d.m.input.Value(); got != "" {
			t.Errorf("%s should not fill the composer, got %q", key, got)
		}
	}
}

// treeDrv builds a family of real session files in a temp dir: a root
// with the given prompts, forked at fork turns (1-based index into
// prompts) into sessions that go on with their own prompts. The
// mounted session is the one named cur ("root", or a fork's name).
// Fork names sort after "root" and in the order given, like UUIDv7s.
func treeDrv(t *testing.T, cur string, root []string, forks ...treeFork) *drv {
	t.Helper()
	dir := t.TempDir()
	write := func(name string, entries []history.Entry) {
		st, err := history.Open(filepath.Join(dir, name+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range entries {
			st.Append(e.Kind, e.Data)
		}
		if err := st.Close(); err != nil {
			t.Fatal(err)
		}
	}
	turnsOf := func(prompts []string) []history.Entry {
		var es []history.Entry
		for _, p := range prompts {
			es = append(es,
				history.Entry{Kind: "input", Data: map[string]any{"text": p}},
				history.Entry{Kind: "done", Data: map[string]any{"files": []string{"a.go"}}})
		}
		return es
	}
	write("root", append([]history.Entry{{Kind: "meta", Data: map[string]any{"cwd": dir}}}, turnsOf(root)...))
	for _, f := range forks {
		// A fork's input seq: meta is 1, turn i's input is 2i.
		src := filepath.Join(dir, f.from+".jsonl")
		if err := history.Fork(src, int64(2*f.at), filepath.Join(dir, f.name+".jsonl")); err != nil {
			t.Fatal(err)
		}
		st, err := history.OpenExisting(filepath.Join(dir, f.name+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		for _, e := range turnsOf(f.prompts) {
			st.Append(e.Kind, e.Data)
		}
		st.Close()
	}
	entries, err := history.Read(filepath.Join(dir, cur+".jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := cfgWith(t, nil, nil, fakeHist{path: filepath.Join(dir, cur+".jsonl"), entries: entries})
	cfg.cmds = reg(t, "tree", "new", "sessions")
	return newDrv(t, 100, 30, cfg)
}

type treeFork struct {
	name, from string
	at         int      // the turn (1-based, in from's prompts) it forks at
	prompts    []string // its own turns
}

// The tree shows every branch of the family, not just this file: the
// root's turns, and a fork's own turns hanging off the turn it left at.
func TestRewindShowsTheWholeFamily(t *testing.T) {
	t.Parallel()
	d := treeDrv(t, "root", []string{"one", "two", "three"},
		treeFork{name: "s1", from: "root", at: 1, prompts: []string{"alt two"}})
	d.press(keyEsc())
	d.press(keyEsc())
	p := d.plain()
	for _, want := range []string{"one", "├─", "alt two", "└─", "two", "three", "(current)", "(continue)"} {
		if !strings.Contains(p, want) {
			t.Errorf("missing %q:\n%s", want, p)
		}
	}
	// The fork hangs off "one" and the trunk reads on below it.
	if strings.Index(p, "alt two") < strings.Index(p, "one") || strings.Index(p, "alt two") > strings.Index(p, "two") {
		t.Errorf("the fork should sit between the turn it left at and the trunk's next turn:\n%s", p)
	}
	// The current path is marked, the side branch is not.
	if !strings.Contains(p, "• three") || strings.Contains(p, "• alt two") {
		t.Errorf("only the current path carries the marker:\n%s", p)
	}
}

// From inside a fork, the origin's turns are the trunk and this
// session is the marked branch; the fork's copied ancestors appear
// once, as the origin's rows.
func TestRewindFromAForkShowsTheOrigin(t *testing.T) {
	t.Parallel()
	d := treeDrv(t, "s1", []string{"one", "two"},
		treeFork{name: "s1", from: "root", at: 1, prompts: []string{"alt two"}})
	d.press(keyEsc())
	d.press(keyEsc())
	p := d.plain()
	if strings.Count(p, "one") != 1 {
		t.Errorf("the shared turn should appear once:\n%s", p)
	}
	if !strings.Contains(p, "• one") || !strings.Contains(p, "• alt two") || strings.Contains(p, "• two") {
		t.Errorf("the marker follows this session's path back to the root:\n%s", p)
	}
	if !strings.Contains(p, "❯ │  • (current)") {
		t.Errorf("the cursor opens on this branch's tip:\n%s", p)
	}
}

// Enter on another branch's turn forks THAT session before the turn,
// naming it, and the prompt lands in the composer.
func TestRewindEnterOnASiblingForksIt(t *testing.T) {
	t.Parallel()
	d := treeDrv(t, "root", []string{"one", "two"},
		treeFork{name: "s1", from: "root", at: 1, prompts: []string{"alt two", "alt three"}})
	d.press(keyEsc())
	d.press(keyEsc())
	// rows: one, ├─ alt two, │ alt three, │ (continue), └─ two, (current)
	for range 3 { // two, (continue), alt three
		d.press(keyUp())
	}
	if !strings.Contains(d.plain(), "❯") || !strings.Contains(d.plain(), "alt three") {
		t.Fatalf("setup:\n%s", d.plain())
	}
	d.press(keyEnter())
	p := d.plain()
	if !strings.Contains(p, "/tree ") || !strings.Contains(p, " s1") {
		t.Errorf("should fork s1 before \"alt three\":\n%s", p)
	}
	if got := d.m.input.Value(); got != "alt three" {
		t.Errorf("composer should hold the prompt rewound past, got %q", got)
	}
}

// Enter on another branch's "(continue)" resumes that session as it
// is: switching branches, not forking.
func TestRewindContinueSwitchesBranch(t *testing.T) {
	t.Parallel()
	d := treeDrv(t, "root", []string{"one", "two"},
		treeFork{name: "s1", from: "root", at: 1, prompts: []string{"alt two"}})
	d.press(keyEsc())
	d.press(keyEsc())
	d.press(keyUp()) // two
	d.press(keyUp()) // (continue) of s1
	if !strings.Contains(d.plain(), "❯ │    (continue)") {
		t.Fatalf("setup:\n%s", d.plain())
	}
	d.press(keyEnter())
	p := d.plain()
	if !strings.Contains(p, "/sessions s1") {
		t.Errorf("(continue) should resume s1:\n%s", p)
	}
	if strings.Contains(p, "/tree") {
		t.Errorf("(continue) must not fork:\n%s", p)
	}
	if got := d.m.input.Value(); got != "" {
		t.Errorf("switching branches leaves the composer alone, got %q", got)
	}
}

// A fork with no turns of its own is not a branch anyone can see into;
// it stays out of the tree unless it is the current session.
func TestRewindHidesEmptyForks(t *testing.T) {
	t.Parallel()
	d := treeDrv(t, "root", []string{"one", "two"},
		treeFork{name: "s1", from: "root", at: 1})
	d.press(keyEsc())
	d.press(keyEsc())
	if p := d.plain(); strings.Contains(p, "├─") {
		t.Errorf("an empty fork should not show:\n%s", p)
	}
	d = treeDrv(t, "s1", []string{"one", "two"},
		treeFork{name: "s1", from: "root", at: 1})
	d.press(keyEsc())
	d.press(keyEsc())
	if p := d.plain(); !strings.Contains(p, "├─ • (current)") {
		t.Errorf("the current session shows even when it has no turns yet:\n%s", p)
	}
}
