package vtreal

// init.js edited while the session is idle: first to a syntax error,
// then to a fixed file with one more command. What must hold: the
// error is surfaced once, the bindings from the last good file keep
// working while the file is broken, and the fixed file reloads (the new
// command runs and /help lists each command exactly once).
//
// Known bug: nothing watches init.js. cmd/bough watchConfig only
// watches the -config file's directory, and plugins/initjs reads the
// init files once, at the init-js row's Apply. So an edit never
// reaches the running session. The reload phases are gated behind
// BOUGH_KNOWN_INITJSHOTEDITBADTHENGOOD.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const initJsHotEditBadThenGoodOne = `
bough.command("hotedit-one", "[x]", "hotedit summary one", function (args) {
	return "HOTEDIT_ONE " + args
})
`

const initJsHotEditBadThenGoodTwo = initJsHotEditBadThenGoodOne + `
bough.command("hotedit-two", "[x]", "hotedit summary two", function (args) {
	return "HOTEDIT_TWO " + args
})
`

// initJsHotEditBadThenGoodKnown skips a phase that hits the known
// no-reload bug unless the gate env var is set.
func initJsHotEditBadThenGoodKnown(t *testing.T) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_INITJSHOTEDITBADTHENGOOD") == "" {
		t.Skip("known bug: init.js edits are never reloaded mid-session (no watcher on ~/.bough/init.js; plugins/initjs reads it only at Apply); set BOUGH_KNOWN_INITJSHOTEDITBADTHENGOOD=1 to run")
	}
}

// initJsHotEditBadThenGoodRun runs /<cmd> <arg> and waits for its reply.
func initJsHotEditBadThenGoodRun(a *app, cmd, arg, want string) {
	a.t.Helper()
	a.typeText("/" + cmd + " " + arg)
	a.key(uv.KeyEnter, 0)
	a.waitFor(want + " " + arg)
	if s := a.settled(); strings.Contains(s, "echo: /"+cmd) {
		a.t.Fatalf("/%s went to the model instead of the command\nscreen:\n%s", cmd, s)
	}
}

// initJsHotEditBadThenGoodPhase runs one phase with a.t pointed at the
// subtest, so a phase failure fails that subtest, not the parent.
func initJsHotEditBadThenGoodPhase(t *testing.T, a *app, name string, f func(t *testing.T)) {
	t.Run(name, func(t *testing.T) {
		parent := a.t
		a.t = t
		defer func() { a.t = parent }()
		f(t)
	})
}

func TestInitJsHotEditBadThenGood(t *testing.T) {
	t.Parallel()
	a := initJsBoot(t, initJsHotEditBadThenGoodOne)
	a.waitFor("say something")
	path := filepath.Join(a.home, ".bough", "init.js")

	initJsHotEditBadThenGoodPhase(t, a, "baseline", func(t *testing.T) {
		initJsHotEditBadThenGoodRun(a, "hotedit-one", "a", "HOTEDIT_ONE")
	})

	if err := os.WriteFile(path, []byte("bough.command(\"hotedit-one\", \n"), 0o644); err != nil {
		t.Fatal(err)
	}

	initJsHotEditBadThenGoodPhase(t, a, "broken edit surfaces one error", func(t *testing.T) {
		initJsHotEditBadThenGoodKnown(t)
		a.waitUntil(func(s string) bool {
			return strings.Contains(s, "init.js") && strings.Contains(s, "SyntaxError")
		}, "a SyntaxError naming init.js after the broken edit")
		time.Sleep(1500 * time.Millisecond) // past any watcher debounce
		if n := strings.Count(a.settled(), "SyntaxError"); n != 1 {
			t.Fatalf("SyntaxError shown %d times, want once\nscreen:\n%s", n, a.text())
		}
	})

	initJsHotEditBadThenGoodPhase(t, a, "old command works while broken", func(t *testing.T) {
		initJsHotEditBadThenGoodRun(a, "hotedit-one", "b", "HOTEDIT_ONE")
		a.check("while init.js is broken")
	})

	if err := os.WriteFile(path, []byte(initJsHotEditBadThenGoodTwo), 0o644); err != nil {
		t.Fatal(err)
	}

	initJsHotEditBadThenGoodPhase(t, a, "fixed file reloads without duplicates", func(t *testing.T) {
		initJsHotEditBadThenGoodKnown(t)
		a.typeText("/help")
		a.key(uv.KeyEnter, 0)
		a.waitFor("hotedit summary two")
		s := a.settled()
		for _, sum := range []string{"hotedit summary one", "hotedit summary two"} {
			if n := strings.Count(s, sum); n != 1 {
				t.Fatalf("/help lists %q %d times, want once\nscreen:\n%s", sum, n, s)
			}
		}
		initJsHotEditBadThenGoodRun(a, "hotedit-two", "c", "HOTEDIT_TWO")
		initJsHotEditBadThenGoodRun(a, "hotedit-one", "d", "HOTEDIT_ONE")
		a.check("after the fixed reload")
	})
}
