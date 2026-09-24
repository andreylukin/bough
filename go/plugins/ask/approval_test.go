package ask

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
)

// events collects what the Asker emits.
func events(a *Asker) chan Event {
	ch := make(chan Event, 16)
	a.emit = func(ev Event) { ch <- ev }
	return ch
}

func next(t *testing.T, ch chan Event) Event {
	t.Helper()
	select {
	case ev := <-ch:
		return ev
	case <-time.After(5 * time.Second):
		t.Fatal("no event")
		return Event{}
	}
}

func none(t *testing.T, ch chan Event, why string) {
	t.Helper()
	select {
	case ev := <-ch:
		t.Fatalf("%s: %s %q", why, ev.Kind, ev.Text)
	case <-time.After(100 * time.Millisecond):
	}
}

// A rule's approval takes the one answer slot the native ask and secret
// share. The engine runs a reply's calls at once: an approval put up
// beside an open ask replaced it on screen, and the answer typed for the
// question the page showed went to the call the child held last. Found
// by tests/model/mbt/rules_prompt_approval_test.go (NativeAsk, Bash).
func TestApprovalWaitsForTheOpenAsk(t *testing.T) {
	t.Parallel()
	reg, a, _, _ := mountNative(t)
	tl, _ := reg.Lookup("ask")
	ch := events(a)
	nat := make(chan agenttools.Result, 1)
	go func() {
		r, _ := tl.Call(context.Background(), agenttools.Call{ID: "n", Args: json.RawMessage(`{"question":"colour?"}`)})
		nat <- r
	}()
	first := next(t, ch)
	approved := make(chan error, 2)
	go func() {
		_, err := a.AskContext(context.Background(), "run c1?", "run", "refuse")
		approved <- err
	}()
	none(t, ch, "an approval was put up beside the open ask")
	if err := a.Answer(first.ID, "red"); err != nil {
		t.Fatal(err)
	}
	if r := <-nat; r.Text != "red" {
		t.Fatalf("native ask = %+v", r)
	}
	second := next(t, ch)
	if second.Kind != "ask" || second.Text != "run c1?" {
		t.Fatalf("after the answer: %+v, want the approval", second)
	}
	// A second approval queues behind the first as well.
	go func() {
		_, err := a.AskContext(context.Background(), "run c2?", "run", "refuse")
		approved <- err
	}()
	none(t, ch, "an approval was put up beside the open approval")
	a.Answer(second.ID, "run")
	if third := next(t, ch); third.Text != "run c2?" {
		t.Fatalf("third = %+v", third)
	}
}

// Stop releases an approval, open or queued: its context is the call's
// (or the block's run), not the codemode VM's, which is Background
// outside a run_js and made an engine approval outlive its turn.
func TestApprovalReleasedByItsContext(t *testing.T) {
	t.Parallel()
	_, a, _, hist := mountNative(t)
	ch := events(a)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 2)
	for _, q := range []string{"open?", "queued?"} {
		go func() {
			_, err := a.AskContext(ctx, q, "run", "refuse")
			done <- err
		}()
		if q == "open?" {
			next(t, ch)
		}
	}
	none(t, ch, "the queued approval was put up")
	cancel()
	for range 2 {
		select {
		case err := <-done:
			if err == nil || !strings.Contains(err.Error(), "cancelled") {
				t.Fatalf("released approval = %v", err)
			}
		case <-time.After(5 * time.Second):
			t.Fatal("an approval outlived its context")
		}
	}
	if es := hist.all(); len(es) != 2 || es[1].Kind != "ask/end" || es[1].Data["id"] != es[0].Data["id"] {
		t.Fatalf("history = %+v, want the open ask's ask/end", es)
	}
}

// An ask that ends with no answer says so, in history and to the UI,
// under its own id: a rule's approval ends as the bash call it gated,
// which names no ask, and the page kept showing it until the turn ended.
func TestAskEndWithoutAnswerIsRecorded(t *testing.T) {
	t.Parallel()
	_, a, _, hist := mountNative(t)
	a.timeout = 50 * time.Millisecond
	ch := events(a)
	_, err := a.AskContext(context.Background(), "run it?", "run", "refuse")
	if err == nil || !strings.Contains(err.Error(), "no answer after") {
		t.Fatalf("timed out approval = %v", err)
	}
	asked := next(t, ch)
	if ev := next(t, ch); ev.Kind != "ask/end" || ev.ID != asked.ID {
		t.Fatalf("end event = %+v, want ask/end for %s", ev, asked.ID)
	}
	es := hist.all()
	if len(es) != 2 || es[1].Kind != "ask/end" || es[1].Data["id"] != asked.ID || es[1].Data["text"] == "" {
		t.Fatalf("history = %+v", es)
	}
}
