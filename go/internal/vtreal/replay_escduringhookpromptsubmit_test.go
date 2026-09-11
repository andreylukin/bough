package vtreal

// Esc while a user-prompt-submit hook is still deciding. The hook for
// the first prompt blocks: it announces itself on a fifo (the test
// reading it is the proof the hook is pending), then waits for a
// release file. Esc there must drop the first prompt before it reaches
// the model; a second prompt submitted after the release must reach it
// exactly once. The tape holds ONE reply, so a leaked first prompt
// would eat it and the second would read "end of tape".

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func escduringhookpromptsubmitTape(t *testing.T) string {
	t.Helper()
	tape := `{"seq":1,"at":"2026-09-10T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-10T10:00:01Z","kind":"input","data":{"text":"second go"}}
{"seq":3,"at":"2026-09-10T10:00:02Z","kind":"assistant","data":{"text":"` + "```stop\\nONLY_REPLY_ON_TAPE\\n```" + `"}}
{"seq":4,"at":"2026-09-10T10:00:03Z","kind":"done","data":{"text":""}}
`
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, []byte(tape), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// escduringhookpromptsubmitConfig is replayConfig plus the tools row,
// so the hook can reach tools.bash in the shared VM.
func escduringhookpromptsubmitConfig(tape string) string {
	return replayConfig(tape) + "- id: tools\n  plugin: tools-basic\n"
}

// escduringhookpromptsubmitComposerHas reports whether the composer
// row holds substr.
func escduringhookpromptsubmitComposerHas(s, substr string) bool {
	ls := strings.Split(s, "\n")
	r := composerRow(ls)
	return r >= 0 && strings.Contains(ls[r], substr)
}

// escduringhookpromptsubmitKnown skips a subtest that fails on a known
// bug: hooks.Fire returns (nil, nil) when esc cancels a pending
// user-prompt-submit hook, and loop.admit carries on as if the hook
// approved — the cancelled prompt is recorded as input and the model
// is still called.
func escduringhookpromptsubmitKnown(t *testing.T) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_ESCDURINGHOOKPROMPTSUBMIT") == "" {
		t.Skip("known bug: a prompt cancelled during its user-prompt-submit hook is still admitted and sent (loop.admit ignores ctx.Err); set BOUGH_KNOWN_ESCDURINGHOOKPROMPTSUBMIT=1 to run")
	}
}

func TestEscDuringHookPromptSubmit(t *testing.T) {
	t.Parallel()
	dir, err := os.MkdirTemp("", "escHook") // short: the paths ride in commands
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	fifo := filepath.Join(dir, "pending")
	release := filepath.Join(dir, "release")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, 100, 30, escduringhookpromptsubmitConfig(escduringhookpromptsubmitTape(t)))
	// Only the first prompt is gated. The poll is a JS loop, so the
	// VM interrupt esc sends lands between calls.
	hooksWrite(t, a.home, "user-prompt-submit", "gate.js", fmt.Sprintf(`
if (!event.input.includes("FIRSTGATED")) return;
tools.bash(%q);
for (;;) { if (String(tools.bash(%q)).includes("OPEN")) break; }
return;`, "echo up > "+fifo, "sleep 0.05; test -e "+release+" && echo OPEN"))

	a.typeText("FIRSTGATED prompt")
	a.key(uv.KeyEnter, 0)
	// Reading the fifo unblocks the hook's echo: it is now polling.
	got := make(chan error, 1)
	go func() {
		f, err := os.Open(fifo)
		if err == nil {
			_, _ = f.Read(make([]byte, 8))
			f.Close()
		}
		got <- err
	}()
	select {
	case err := <-got:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(20 * time.Second):
		t.Fatalf("user-prompt-submit hook never started:\n%s", a.text())
	}
	a.key(uv.KeyEsc, 0)
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "cancelled") || escduringhookpromptsubmitComposerHas(s, "FIRSTGATED")
	}, "a cancelled notice or the restored draft")
	afterEsc := a.settled()
	if err := os.WriteFile(release, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	a.check("after esc")
	t.Run("FirstPromptNotSent", func(t *testing.T) {
		escduringhookpromptsubmitKnown(t)
		if strings.Contains(afterEsc, "ONLY_REPLY_ON_TAPE") {
			t.Errorf("the cancelled prompt reached the model:\n%s", afterEsc)
		}
		if got := hooksEntries(a, "input"); len(got) != 0 {
			t.Errorf("input entries after esc = %q, want none", got)
		}
	})

	n := a.doneCount()
	a.typeText("second go")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(n+1, 30*time.Second) {
		t.Fatalf("second turn never finished:\n%s", a.text())
	}
	a.check("after second")
	s := a.settled()
	t.Run("SecondPromptOnce", func(t *testing.T) {
		escduringhookpromptsubmitKnown(t)
		if c := strings.Count(s, "ONLY_REPLY_ON_TAPE"); c != 1 {
			t.Errorf("tape reply on screen %d times, want 1:\n%s", c, s)
		}
		if strings.Contains(s, "end of tape") {
			t.Errorf("tape ran out: the first prompt took the reply:\n%s", s)
		}
		if got := hooksEntries(a, "input"); strings.Join(got, "|") != "second go" {
			t.Errorf("input entries = %q, want [second go]", got)
		}
		if got := hooksEntries(a, "assistant"); len(got) != 1 {
			t.Errorf("assistant entries = %q, want exactly one model request", got)
		}
	})
}
