package vtreal

// Kitty keyboard protocol with report-all-keys (flags 1|2|8|16: every
// key as CSI-u, press AND release events, associated text) driven
// through the overlays: the @ picker, the /model picker, a pending
// ask and the "/" palette. A release event must never type a
// character, and an enter or esc release must never fire a second time.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// kittyReleaseEventsInPickersStart boots bough with yml under a kitty TERM.
func kittyReleaseEventsInPickersStart(t *testing.T, cols, rows int, yml string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-kitty", "KITTY_WINDOW_ID=1", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// kittyReleaseEventsInPickersKey sends one key as a report-all-keys
// terminal does: the press (with its associated text, when it has one)
// then the release. mod is the CSI-u modifier value (1 = none).
func kittyReleaseEventsInPickersKey(a *app, code, mod int, text string) {
	c, m := strconv.Itoa(code), strconv.Itoa(mod)
	press := "\x1b[" + c + ";" + m + "u"
	if text != "" {
		press = "\x1b[" + c + ";" + m + ";" + strconv.Itoa(int([]rune(text)[0])) + "u"
	}
	kittyKeyboardProtocolRaw(a, press)
	kittyKeyboardProtocolRaw(a, "\x1b["+c+";"+m+":3u")
}

// kittyReleaseEventsInPickersType types s, every rune pressed and released.
func kittyReleaseEventsInPickersType(a *app, s string) {
	for _, r := range s {
		kittyReleaseEventsInPickersKey(a, int(r), 1, string(r))
	}
}

func kittyReleaseEventsInPickersEnter(a *app) { kittyReleaseEventsInPickersKey(a, 13, 1, "") }
func kittyReleaseEventsInPickersEsc(a *app)   { kittyReleaseEventsInPickersKey(a, 27, 1, "") }

// kittyReleaseEventsInPickersDraft waits for the composer row to be want.
func kittyReleaseEventsInPickersDraft(a *app, where, want string) {
	a.t.Helper()
	a.waitUntil(func(string) bool { return kittyKeyboardProtocolComposer(a) == want }, where+": composer "+want)
}

func TestKittyReleaseEventsInPickers(t *testing.T) {
	t.Parallel()

	t.Run("at_picker", func(t *testing.T) {
		t.Parallel()
		a := kittyReleaseEventsInPickersStart(t, 100, 30, config)
		for name, body := range atPickerFiles {
			p := filepath.Join(a.home, name)
			if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		kittyReleaseEventsInPickersType(a, "@main")
		a.atPickerWant("filtered", []string{"src/main.go"}, "src/main.go")
		kittyReleaseEventsInPickersDraft(a, "typed", "> @main")
		// Enter completes; its release must not submit the draft.
		kittyReleaseEventsInPickersEnter(a)
		a.atPickerDraft("completed", "> @src/main.go")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "echo:") {
			t.Fatalf("enter release submitted the completed draft:\n%s", s)
		}
		// Esc closes a reopened picker; its release must not arm the
		// clear-draft prompt.
		kittyReleaseEventsInPickersType(a, "@RE") // completion left a trailing space
		a.atPickerWant("reopened", []string{"README.md"}, "README.md")
		kittyReleaseEventsInPickersEsc(a)
		a.atPickerWant("closed", nil, "")
		s = a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "press esc again") {
			t.Fatalf("esc release counted as a second esc:\n%s", s)
		}
		if got := kittyKeyboardProtocolComposer(a); got != "> @src/main.go @RE" {
			t.Fatalf("composer = %q after picker esc:\n%s", got, s)
		}
	})

	t.Run("model_picker", func(t *testing.T) {
		t.Parallel()
		tape, _ := filepath.Abs("testdata/replay/basic.jsonl")
		a := kittyReleaseEventsInPickersStart(t, 120, 30, replayConfig(tape))
		kittyReleaseEventsInPickersType(a, "/model")
		kittyReleaseEventsInPickersEnter(a)
		a.waitFor("pick a model")
		kittyReleaseEventsInPickersType(a, "echo")
		a.waitFor("▸ llm-echo")
		kittyKeyboardProtocolNoLeak(t, a.settled())
		// Esc closes; its release must not arm the clear prompt.
		kittyReleaseEventsInPickersEsc(a)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "picker to close")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "press esc again") {
			t.Fatalf("esc release counted as a second esc:\n%s", s)
		}
		// Reopen and pick: the enter release must not run a turn.
		kittyReleaseEventsInPickersType(a, "/model")
		kittyReleaseEventsInPickersEnter(a)
		a.waitFor("pick a model")
		kittyReleaseEventsInPickersType(a, "echo")
		a.waitFor("▸ llm-echo")
		kittyReleaseEventsInPickersEnter(a)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "picker to close on enter")
		s = a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if n := a.modelEffortAssistants(); n != 0 {
			t.Fatalf("enter release in the model picker ran a turn (%d replies):\n%s", n, s)
		}
		if got := kittyKeyboardProtocolComposer(a); got != "" && !strings.Contains(got, "say something") {
			t.Fatalf("composer = %q after picking a model:\n%s", got, s)
		}
	})

	t.Run("palette", func(t *testing.T) {
		t.Parallel()
		a := kittyReleaseEventsInPickersStart(t, 100, 30, config)
		// ctrl+x p via CSI-u, each with its release.
		kittyReleaseEventsInPickersKey(a, 'x', 5, "")
		a.waitFor("ctrl+x …")
		kittyReleaseEventsInPickersKey(a, 'p', 1, "p")
		a.waitFor("collapse_all")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if got := kittyKeyboardProtocolComposer(a); got != "> /" {
			t.Fatalf("composer = %q after ctrl+x p (release typed?):\n%s", got, s)
		}
		kittyReleaseEventsInPickersType(a, "coll")
		kittyReleaseEventsInPickersDraft(a, "filtered", "> /coll")
		kittyReleaseEventsInPickersEsc(a)
		a.waitUntil(func(string) bool { return strings.Contains(kittyKeyboardProtocolComposer(a), "say something") }, "esc to close the palette")
		s = a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "press esc again") {
			t.Fatalf("esc release counted as a second esc:\n%s", s)
		}
		// Slash palette: "/help" + enter; the release must not send a turn.
		kittyReleaseEventsInPickersType(a, "/help")
		kittyReleaseEventsInPickersEnter(a)
		a.waitUntil(func(string) bool { return !strings.Contains(kittyKeyboardProtocolComposer(a), "/help") }, "/help to run")
		s = a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "echo:") {
			t.Fatalf("enter release sent a turn after /help:\n%s", s)
		}
	})

	t.Run("ask", func(t *testing.T) {
		t.Parallel()
		tape, err := filepath.Abs("testdata/replay/ask.jsonl")
		if err != nil {
			t.Fatal(err)
		}
		a := kittyReleaseEventsInPickersStart(t, 100, 30, askConfig(tape))
		kittyReleaseEventsInPickersType(a, "pick a color")
		kittyReleaseEventsInPickersEnter(a)
		a.waitFor("? Pick a color")
		a.waitFor(askPendingPlaceholder)
		// Freeform with a shift+enter newline: the ask stays pending.
		kittyReleaseEventsInPickersType(a, "octa")
		kittyReleaseEventsInPickersKey(a, 13, 2, "")
		kittyReleaseEventsInPickersType(a, "rine")
		a.waitFor("rine")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "→") || strings.Contains(s, "Color locked in.") {
			t.Fatalf("shift+enter answered the ask:\n%s", s)
		}
		if strings.Contains(s, "octarine") {
			t.Fatalf("shift+enter did not break the freeform line:\n%s", s)
		}
		kittyReleaseEventsInPickersEnter(a)
		a.waitUntil(func(s string) bool {
			return strings.Contains(s, "Color locked in.") && strings.Contains(s, "❯? Pick a color → octa")
		}, "the answered ask and the next tape reply")
		s = a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "waiting for you") {
			t.Fatalf("still waiting after the answer:\n%s", s)
		}
		if got := kittyKeyboardProtocolComposer(a); strings.Contains(got, "rine") || strings.Contains(got, "octa") {
			t.Fatalf("answer left in the composer (%q):\n%s", got, s)
		}
	})
}
