package vtreal

// Hooks that misbehave: a pre-code-exec hook that busy-sleeps 5s must
// not make esc hang, and hooks that throw (session-start and
// pre-code-exec) must neither kill the turn nor fail silently — the
// user has to see that their hook broke.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// hooksSlowAndFailingTape writes a one-turn tape: one js block, then a
// stop reply.
func hooksSlowAndFailingTape(t *testing.T, input, code, result, reply string) string {
	t.Helper()
	q := func(s string) string { return strings.ReplaceAll(strings.ReplaceAll(s, `\`, `\\`), "\n", `\n`) }
	tape := `{"seq":1,"at":"2026-09-10T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}
{"seq":2,"at":"2026-09-10T10:00:01Z","kind":"input","data":{"text":"` + input + `"}}
{"seq":3,"at":"2026-09-10T10:00:02Z","kind":"assistant","data":{"text":"` + q("```js\n"+code+"```") + `"}}
{"seq":4,"at":"2026-09-10T10:00:02Z","kind":"code","data":{"text":"` + q(code) + `"}}
{"seq":5,"at":"2026-09-10T10:00:02Z","kind":"result","data":{"code":"` + q(code) + `","text":"` + result + `"}}
{"seq":6,"at":"2026-09-10T10:00:03Z","kind":"assistant","data":{"text":"` + q("```stop\n"+reply+"\n```") + `"}}
{"seq":7,"at":"2026-09-10T10:00:03Z","kind":"done","data":{"text":""}}
`
	p := filepath.Join(t.TempDir(), "tape.jsonl")
	if err := os.WriteFile(p, []byte(tape), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestHooksSlowAndFailingEscDuringSleepingHook(t *testing.T) {
	t.Parallel()
	tape := hooksSlowAndFailingTape(t, "run the slow block", "SLEEPY_BLOCK()\n", "SLEEPY_RECORDED_OUTPUT", "Slow turn over.")
	a := startCfg(t, 100, 30, replayConfig(tape))
	hooksWrite(t, a.home, "pre-code-exec", "slow.js",
		`var t = Date.now() + 5000; while (Date.now() < t) {}`)

	a.typeText("run the slow block")
	a.key(uv.KeyEnter, 0)
	// The block renders before pre-code-exec fires: from here the turn
	// is waiting on the sleeping hook.
	a.waitFor("SLEEPY_BLOCK")
	time.Sleep(300 * time.Millisecond)
	a.key(uv.KeyEsc, 0)
	start := time.Now()
	if !a.waitDone(1, 20*time.Second) {
		t.Fatalf("turn never ended after esc during a sleeping hook:\n%s", a.text())
	}
	if d := time.Since(start); d > 3*time.Second {
		t.Errorf("esc took %s to end the turn; the sleeping hook blocked the cancel", d)
	}
	a.waitFor("■ cancelled")
	s := a.settled()
	if strings.Contains(s, "SLEEPY_RECORDED_OUTPUT") || strings.Contains(s, "Slow turn over.") {
		t.Errorf("the block ran after esc:\n%s", s)
	}
	a.check("after esc during hook")
}

func TestHooksSlowAndFailingThrowingHookIsVisible(t *testing.T) {
	t.Parallel()
	tape := hooksSlowAndFailingTape(t, "run the ok block", "console.log('ok')\n", "OK_RECORDED_OUTPUT", "Throw turn over.")
	a := startCfg(t, 100, 30, replayConfig(tape))
	hooksWrite(t, a.home, "session-start", "boom.js", `throw new Error("SESSION_HOOK_BOOM")`)
	hooksWrite(t, a.home, "pre-code-exec", "boom.js", `throw new Error("PRE_HOOK_BOOM")`)

	a.typeText("run the ok block")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished with throwing hooks:\n%s", a.text())
	}
	a.check("after throwing hooks")
	s := a.settled()
	t.Run("TurnSurvives", func(t *testing.T) {
		for _, want := range []string{"OK_RECORDED_OUTPUT", "Throw turn over."} {
			if !strings.Contains(s, want) {
				t.Errorf("%q not on screen:\n%s", want, s)
			}
		}
	})
	t.Run("ErrorVisible", func(t *testing.T) {
		for _, want := range []string{"SESSION_HOOK_BOOM", "PRE_HOOK_BOOM"} {
			if !strings.Contains(s, want) {
				t.Errorf("throwing hook error %q not on screen:\n%s", want, s)
			}
		}
	})
}
