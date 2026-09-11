package vtreal

// The pinned todo panel across a rewind and a resume. A session whose
// three turns each change the list is resumed; double esc rewinds to
// before turn 3 (a fork that keeps turns 1-2). The panel must show the
// turn-2 list — live, right after the rewind, and again when bough is
// restarted on the forked session file — never turn 3's. A panel put
// away with ctrl+t stays put away across the rewind.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// todoPanelWithRewindAndResumeSession is three turns of todo changes.
// After turn 2: [x] 1, [ ] 2, [ ] 3. Turn 3 adds 4 and checks off 2.
const todoPanelWithRewindAndResumeSession = `{"seq":1,"at":"2026-09-10T11:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-10T11:00:01Z","kind":"input","data":{"text":"plan the release"}}
{"seq":3,"at":"2026-09-10T11:00:02Z","kind":"assistant","data":{"text":"` + "```stop\\nPLANNED\\n```" + `"}}
{"seq":4,"at":"2026-09-10T11:00:02Z","kind":"todo/add","data":{"id":1,"text":"cut the tag"}}
{"seq":5,"at":"2026-09-10T11:00:02Z","kind":"todo/add","data":{"id":2,"text":"review the diff"}}
{"seq":6,"at":"2026-09-10T11:00:03Z","kind":"done","data":{"text":""}}
{"seq":7,"at":"2026-09-10T11:01:00Z","kind":"input","data":{"text":"start the work"}}
{"seq":8,"at":"2026-09-10T11:01:01Z","kind":"assistant","data":{"text":"` + "```stop\\nSTARTED\\n```" + `"}}
{"seq":9,"at":"2026-09-10T11:01:01Z","kind":"todo/done","data":{"id":1}}
{"seq":10,"at":"2026-09-10T11:01:01Z","kind":"todo/add","data":{"id":3,"text":"write the notes"}}
{"seq":11,"at":"2026-09-10T11:01:02Z","kind":"done","data":{"text":""}}
{"seq":12,"at":"2026-09-10T11:02:00Z","kind":"input","data":{"text":"finish up"}}
{"seq":13,"at":"2026-09-10T11:02:01Z","kind":"assistant","data":{"text":"` + "```stop\\nFINISHED\\n```" + `"}}
{"seq":14,"at":"2026-09-10T11:02:01Z","kind":"todo/add","data":{"id":4,"text":"ship it"}}
{"seq":15,"at":"2026-09-10T11:02:01Z","kind":"todo/done","data":{"id":2}}
{"seq":16,"at":"2026-09-10T11:02:02Z","kind":"done","data":{"text":""}}
`

var (
	todoPanelWithRewindAndResumeTurn2 = []string{"[x] 1 cut the tag", "[ ] 2 review the diff", "[ ] 3 write the notes"}
	todoPanelWithRewindAndResumeTurn3 = []string{"ship it", "[x] 2 review the diff"}
)

// todoPanelWithRewindAndResumeStart boots bough with the history row
// resuming session (the replay rows read it too; no model call is made).
func todoPanelWithRewindAndResumeStart(t *testing.T, session string) *app {
	t.Helper()
	yml := strings.Replace(replayConfig(session),
		"- id: history\n  plugin: history",
		"- id: history\n  plugin: history\n  config: {file: "+session+"}", 1)
	if !strings.Contains(yml, "config: {file: "+session) {
		t.Fatalf("could not point the history row at %s:\n%s", session, yml)
	}
	return startCfg(t, 100, 30, yml)
}

// todoPanelWithRewindAndResumeFork waits for the rewind's fork: the one
// session file in dir other than src.
func todoPanelWithRewindAndResumeFork(a *app, dir, src string) string {
	a.t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		paths, _ := filepath.Glob(filepath.Join(dir, "*.jsonl"))
		for _, p := range paths {
			if p != src {
				return p
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.t.Fatalf("rewind wrote no forked session in %s:\n%s", dir, a.text())
	return ""
}

// todoPanelWithRewindAndResumeWant asserts the panel shows the turn-2
// list and nothing turn 3 did.
func todoPanelWithRewindAndResumeWant(t *testing.T, where, s string) {
	t.Helper()
	if !strings.Contains(s, todoHeader) {
		t.Fatalf("%s: todo panel not shown:\n%s", where, s)
	}
	for _, want := range todoPanelWithRewindAndResumeTurn2 {
		if !strings.Contains(s, want) {
			t.Errorf("%s: panel lacks turn-2 item %q:\n%s", where, want, s)
		}
	}
	for _, bad := range todoPanelWithRewindAndResumeTurn3 {
		if strings.Contains(s, bad) {
			t.Errorf("%s: panel still shows turn 3's %q:\n%s", where, bad, s)
		}
	}
}

// todoPanelWithRewindAndResumeRewind opens the rewind menu and picks
// "finish up" (turn 3): back to the conversation after turn 2.
func todoPanelWithRewindAndResumeRewind(a *app) {
	a.t.Helper()
	a.key(uv.KeyEscape, 0)
	a.waitFor("press esc again to rewind")
	a.key(uv.KeyEscape, 0)
	a.waitFor("(current)")
	a.key(uv.KeyUp, 0)
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(string) bool {
		ls := a.lines()
		r := composerRow(ls)
		return r >= 0 && strings.Contains(ls[r], "finish up")
	}, "the rewound prompt in the composer")
}

// todoPanelWithRewindAndResumeSetup writes the session where bough keeps
// sessions ($HOME/.bough/history): a rewind forks next to the source
// file and then resumes the fork BY ID from that directory.
func todoPanelWithRewindAndResumeSetup(t *testing.T) (home, dir, session string) {
	t.Helper()
	home = t.TempDir()
	dir = filepath.Join(home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	session = filepath.Join(dir, "todo-rewind.jsonl")
	if err := os.WriteFile(session, []byte(todoPanelWithRewindAndResumeSession), 0o644); err != nil {
		t.Fatal(err)
	}
	return home, dir, session
}

// todoPanelWithRewindAndResumeBoot starts bough in home resuming session.
func todoPanelWithRewindAndResumeBoot(t *testing.T, home, session string) *app {
	t.Helper()
	return undoStart(t, home, 100, 30, undoConfig(t, session))
}

func TestTodoPanelWithRewindAndResume(t *testing.T) {
	t.Parallel()
	home, dir, session := todoPanelWithRewindAndResumeSetup(t)
	a := todoPanelWithRewindAndResumeBoot(t, home, session)

	t.Run("ResumedShowsTurn3", func(t *testing.T) {
		a.waitFor("[ ] 4 ship it")
		s := a.settled()
		if !strings.Contains(s, "[x] 2 review the diff") {
			t.Fatalf("resumed panel is not the turn-3 list:\n%s", s)
		}
		a.check("resumed at turn 3")
	})

	var fork string
	t.Run("RewindLiveShowsTurn2", func(t *testing.T) {
		todoPanelWithRewindAndResumeRewind(a)
		fork = todoPanelWithRewindAndResumeFork(a, dir, session)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "ship it") }, "turn 3's item to leave the panel")
		todoPanelWithRewindAndResumeWant(t, "live after rewind", a.settled())
		a.check("after rewind")
	})

	t.Run("ResumeForkShowsTurn2", func(t *testing.T) {
		if fork == "" {
			t.Skip("no fork from the rewind subtest")
		}
		b := todoPanelWithRewindAndResumeStart(t, fork)
		b.waitFor(todoHeader)
		todoPanelWithRewindAndResumeWant(t, "resumed fork", b.settled())
		b.check("resumed fork")

		// ctrl+t still toggles the resumed panel, and brings back the
		// turn-2 list, not a stale one.
		b.key('t', uv.ModCtrl)
		b.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the panel to hide")
		b.key('t', uv.ModCtrl)
		b.waitFor(todoHeader)
		todoPanelWithRewindAndResumeWant(t, "resumed fork after ctrl+t twice", b.settled())
	})
}

// A panel hidden with ctrl+t stays hidden across the rewind (the list
// under it changes, the pin choice does not), and ctrl+t then shows
// the turn-2 list.
func TestTodoPanelWithRewindAndResumeHiddenStaysHidden(t *testing.T) {
	t.Parallel()
	home, dir, session := todoPanelWithRewindAndResumeSetup(t)
	a := todoPanelWithRewindAndResumeBoot(t, home, session)
	a.waitFor(todoHeader)
	a.key('t', uv.ModCtrl)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the panel to hide")

	todoPanelWithRewindAndResumeRewind(a)
	todoPanelWithRewindAndResumeFork(a, dir, session)
	if s := a.settled(); strings.Contains(s, todoHeader) || strings.Contains(s, "[ ] 3 write the notes") {
		t.Fatalf("rewind re-pinned a panel ctrl+t had hidden:\n%s", s)
	}
	a.check("hidden after rewind")

	a.key('t', uv.ModCtrl)
	a.waitFor(todoHeader)
	todoPanelWithRewindAndResumeWant(t, "ctrl+t after rewind", a.settled())
	a.check("shown after rewind")
}
