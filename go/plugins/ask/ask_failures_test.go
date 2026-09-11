package ask

// Failure-path tests for tools.ask driven through the REAL goja
// codemode VM, so each case checks what the model's js block gets back
// (return value or thrown error) and that the run ends within a bound.
// Known product bugs are gated behind BOUGH_KNOWN_TOOLS_ASK_TODO_SCRATCH=1.

import (
	"context"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	cmpkg "github.com/andreylukin/bough/plugins/codemode"
)

func askFailKnownBug(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_ASK_TODO_SCRATCH") != "1" {
		t.Skip("known bug (set BOUGH_KNOWN_TOOLS_ASK_TODO_SCRATCH=1 to run): " + bug)
	}
}

// askFailMount mounts the ask plugin on a real CodeMode with the given
// script timeout; every emitted ask event lands on the channel.
func askFailMount(t *testing.T, jsTimeout time.Duration) (*cmpkg.CodeMode, *Asker, chan Event) {
	t.Helper()
	cm := cmpkg.New(jsTimeout)
	ctx := kernel.NewContext()
	ctx.Provide("codemode", cm)
	events := make(chan Event, 256)
	ctx.On("loop/event", func(p any) {
		if ev, ok := p.(Event); ok {
			events <- ev
		}
	})
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("apply: %v", err)
	}
	a, err := kernel.Get[*Asker](ctx, "ask-answers")
	if err != nil {
		t.Fatal(err)
	}
	return cm, a, events
}

type askFailRes struct {
	out string
	err error
}

// askFailRun runs code in the background and returns its result channel.
func askFailRun(cm *cmpkg.CodeMode, ctx context.Context, code string) chan askFailRes {
	ch := make(chan askFailRes, 1)
	go func() {
		out, err := cm.RunCtx(ctx, code)
		ch <- askFailRes{out, err}
	}()
	return ch
}

func askFailWait(t *testing.T, ch chan askFailRes, bound time.Duration) askFailRes {
	t.Helper()
	select {
	case r := <-ch:
		return r
	case <-time.After(bound):
		t.Fatalf("run did not finish within %s", bound)
		return askFailRes{}
	}
}

func askFailEvent(t *testing.T, events chan Event) Event {
	t.Helper()
	select {
	case ev := <-events:
		return ev
	case <-time.After(5 * time.Second):
		t.Fatal("no ask event emitted")
		return Event{}
	}
}

func TestAskFailuresShapes(t *testing.T) {
	long := strings.Repeat("why? ", 20000) // 100 kB question
	cases := []struct {
		name, code string
		wantQ      string
		wantOpts   int
	}{
		{"no options", `tools.ask("proceed?")`, "proceed?", 0},
		{"100 options", `var o=[]; for (var i=0;i<100;i++) o.push("opt"+i); tools.ask("pick", ...o)`, "pick", 100},
		{"very long question", `tools.ask(` + "`" + long + "`" + `)`, long, 0},
		{"options as one array arg", `tools.ask("pick", ["a","b"])`, "pick", 1},
		{"unicode", `tools.ask("🌳 δέντρο? 木", "да", "لا")`, "🌳 δέντρο? 木", 2},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			cm, a, events := askFailMount(t, 5*time.Second)
			ch := askFailRun(cm, t.Context(), c.code)
			ev := askFailEvent(t, events)
			if ev.Text != c.wantQ || len(ev.Options) != c.wantOpts {
				t.Errorf("event q=%.40q opts=%d %q, want q=%.40q opts=%d", ev.Text, len(ev.Options), ev.Options, c.wantQ, c.wantOpts)
			}
			if err := a.Answer(ev.ID, "ans"); err != nil {
				t.Fatal(err)
			}
			r := askFailWait(t, ch, 5*time.Second)
			if r.err != nil || r.out != "ans" {
				t.Fatalf("got %q, %v; want ans", r.out, r.err)
			}
		})
	}
}

// Odd argument types must not panic the VM; the call either asks (with
// stringified text) or throws a catchable error.
func TestAskFailuresOddArgs(t *testing.T) {
	for _, code := range []string{
		`tools.ask()`,
		`tools.ask(42, {a:1}, null, undefined)`,
		`tools.ask(undefined)`,
	} {
		t.Run(code, func(t *testing.T) {
			cm, a, events := askFailMount(t, 5*time.Second)
			ch := askFailRun(cm, t.Context(), `try { `+code+` } catch (e) { "threw:" + e }`)
			select {
			case r := <-ch:
				if r.err != nil {
					t.Fatalf("uncaught: %v", r.err)
				}
				if !strings.HasPrefix(r.out, "threw:") {
					t.Fatalf("returned %q without asking", r.out)
				}
				t.Logf("threw: %s", r.out)
			case ev := <-events:
				t.Logf("asked %q opts=%q", ev.Text, ev.Options)
				_ = a.Answer(ev.ID, "ok")
				r := askFailWait(t, ch, 5*time.Second)
				if r.err != nil || r.out != "ok" {
					t.Fatalf("got %q, %v", r.out, r.err)
				}
			case <-time.After(5 * time.Second):
				t.Fatal("neither asked nor returned")
			}
		})
	}
}

// Esc in the TUI answers "(declined)" (ui/model.go): the block gets it as
// an ordinary return value and keeps running.
func TestAskFailuresDeclined(t *testing.T) {
	cm, a, events := askFailMount(t, 5*time.Second)
	ch := askFailRun(cm, t.Context(), `var r = tools.ask("deploy?", "yes", "no"); r === "(declined)" ? "skipped" : "went:" + r`)
	ev := askFailEvent(t, events)
	if err := a.Answer(ev.ID, "(declined)"); err != nil {
		t.Fatal(err)
	}
	if r := askFailWait(t, ch, 5*time.Second); r.err != nil || r.out != "skipped" {
		t.Fatalf("got %q, %v", r.out, r.err)
	}
	if err := a.Answer(ev.ID, "again"); err == nil {
		t.Fatal("a second answer to a resolved ask should error")
	}
}

// The ask timeout (seconds-level via the timeout field) is a thrown,
// catchable error; the pending entry is gone afterwards.
func TestAskFailuresTimeoutThrowsInJS(t *testing.T) {
	cm, a, _ := askFailMount(t, 5*time.Second)
	a.timeout = 100 * time.Millisecond
	r := askFailWait(t, askFailRun(cm, t.Context(), `try { tools.ask("hello?") } catch (e) { "caught: " + e.message }`), 5*time.Second)
	if r.err != nil || !strings.Contains(r.out, "no answer after") {
		t.Fatalf("got %q, %v", r.out, r.err)
	}
	r = askFailWait(t, askFailRun(cm, t.Context(), `tools.ask("hello?")`), 5*time.Second)
	if r.err == nil || !strings.Contains(r.err.Error(), "no answer after") {
		t.Fatalf("uncaught timeout: %q, %v", r.out, r.err)
	}
	a.mu.Lock()
	n := len(a.pending)
	a.mu.Unlock()
	if n != 0 {
		t.Fatalf("%d pending asks leaked", n)
	}
}

// Cancel (headless stdin closed) throws a catchable error.
func TestAskFailuresCancelThrowsInJS(t *testing.T) {
	cm, a, events := askFailMount(t, 5*time.Second)
	ch := askFailRun(cm, t.Context(), `try { tools.ask("q?") } catch (e) { "caught: " + e.message }`)
	ev := askFailEvent(t, events)
	if err := a.Cancel(ev.ID); err != nil {
		t.Fatal(err)
	}
	if r := askFailWait(t, ch, 5*time.Second); r.err != nil || !strings.Contains(r.out, "cancelled") {
		t.Fatalf("got %q, %v", r.out, r.err)
	}
}

// Waiting on the user must not trip the script timeout, and the
// timeout is re-armed after the answer (a loop after the ask still dies).
func TestAskFailuresPauseAndRearm(t *testing.T) {
	cm, a, events := askFailMount(t, 300*time.Millisecond)
	ch := askFailRun(cm, t.Context(), `var r = tools.ask("slow?"); console.log(r); while (true) {}`)
	ev := askFailEvent(t, events)
	time.Sleep(700 * time.Millisecond) // > script timeout while blocked
	if err := a.Answer(ev.ID, "late but fine"); err != nil {
		t.Fatalf("answer after >timeout wait: %v", err)
	}
	r := askFailWait(t, ch, 5*time.Second)
	if r.err == nil || !strings.Contains(r.err.Error(), "timeout") {
		t.Fatalf("infinite loop after ask should time out, got %q, %v", r.out, r.err)
	}
	if !strings.Contains(r.out, "late but fine") {
		t.Fatalf("answer lost: %q", r.out)
	}
}

// Two asks pending at once (the harness's own Ask while a block's ask
// is up) resolve independently, answered out of order.
func TestAskFailuresConcurrent(t *testing.T) {
	_, a, events := askFailMount(t, 5*time.Second)
	var wg sync.WaitGroup
	got := make([]string, 2)
	for i, q := range []string{"first?", "second?"} {
		wg.Add(1)
		go func() {
			defer wg.Done()
			s, err := a.Ask(q)
			if err != nil {
				t.Errorf("%s: %v", q, err)
			}
			got[i] = s
		}()
	}
	byQ := map[string]string{}
	for range 2 {
		ev := askFailEvent(t, events)
		byQ[ev.Text] = ev.ID
	}
	if byQ["first?"] == byQ["second?"] {
		t.Fatal("two asks share an id")
	}
	_ = a.Answer(byQ["second?"], "B")
	_ = a.Answer(byQ["first?"], "A")
	wg.Wait()
	if got[0] != "A" || got[1] != "B" {
		t.Fatalf("answers crossed: %v", got)
	}
}

// A subagent's block runs nested in the parent's VM, with the parent's
// timer paused by workers (workers.go Pause). Its ask must survive a
// wait longer than the script timeout, and the parent block completes.
func TestAskFailuresFromNestedSubagentRun(t *testing.T) {
	cm, a, events := askFailMount(t, 300*time.Millisecond)
	cm.RegisterTool("sub", func() (string, error) {
		defer cm.Pause()()
		return cm.Run(`"child got " + tools.ask("child question?", "x", "y")`)
	})
	ch := askFailRun(cm, t.Context(), `"parent saw: " + tools.sub()`)
	ev := askFailEvent(t, events)
	time.Sleep(800 * time.Millisecond)
	if err := a.Answer(ev.ID, "y"); err != nil {
		t.Fatal(err)
	}
	r := askFailWait(t, ch, 5*time.Second)
	if r.err != nil || r.out != "parent saw: child got y" {
		t.Fatalf("got %q, %v", r.out, r.err)
	}
}

// Cancelling the turn (ctrl+c; loop/cancel.go runCode cancels ctx,
// interrupts the VM, then WAITS for the run) while an ask is pending
// must end the block promptly. ask() selects only on the answer channel
// and its own timeout, never on the run's context, so the turn hangs
// until the ask timeout (10 min by default).
func TestAskFailuresTurnCancelReleasesPendingAsk(t *testing.T) {
	cm, a, events := askFailMount(t, 5*time.Second)
	a.timeout = 20 * time.Second
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	ch := askFailRun(cm, ctx, `tools.ask("still there?")`)
	ev := askFailEvent(t, events)
	defer a.Cancel(ev.ID) // release the goroutine if the bug holds
	cancel()
	cm.Interrupt() // what runCode does after ctx.Done
	r := askFailWait(t, ch, 3*time.Second)
	if r.err == nil {
		t.Fatalf("cancelled run returned %q without error", r.out)
	}
}

// tools.ask() with no question (or a null/undefined one) puts an empty
// "? " prompt in front of the user with nothing to answer; Add-style
// validation (todo rejects empty text) should throw instead.
func TestAskFailuresEmptyQuestionRejected(t *testing.T) {
	cm, a, events := askFailMount(t, 5*time.Second)
	ch := askFailRun(cm, t.Context(), `tools.ask()`)
	select {
	case r := <-ch:
		if r.err == nil {
			t.Fatalf("empty ask returned %q without error", r.out)
		}
	case ev := <-events:
		_ = a.Cancel(ev.ID)
		t.Fatalf("empty question was put to the user (id %s)", ev.ID)
	case <-time.After(5 * time.Second):
		t.Fatal("hung")
	}
}
