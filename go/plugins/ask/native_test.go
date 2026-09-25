package ask

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

func mountNative(t *testing.T) (agenttools.Registry, *Asker, *fakeCode, *fakeHist) {
	t.Helper()
	ctx := kernel.NewContext()
	code, hist := &fakeCode{}, &fakeHist{}
	reg := agenttools.NewRegistry()
	ctx.Provide("codemode", code)
	ctx.Provide("history", hist)
	ctx.Provide("agent-tools", reg)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	a, _ := kernel.Get[*Asker](ctx, "ask-answers")
	return reg, a, code, hist
}

// Native ask is the same question and the same two history entries as
// tools.ask; it blocks its call (Blocking, so no settle and no call
// timeout) and never touches codemode's timer.
func TestNativeAskRoundTrip(t *testing.T) {
	t.Parallel()
	reg, a, code, hist := mountNative(t)
	tl, ok := reg.Lookup("ask")
	if !ok || !tl.Blocking {
		t.Fatalf("native ask missing or not blocking: %+v", tl)
	}
	if d := tl.Detail(json.RawMessage(`{"question":"ship it?"}`)); d != "ship it?" {
		t.Fatalf("detail = %q", d)
	}
	a.emit = func(ev Event) {
		if strings.Join(ev.Options, "|") != "yes|no" {
			t.Errorf("options = %v", ev.Options)
		}
		go a.Answer(ev.ID, "yes")
	}
	r, err := tl.Call(context.Background(), agenttools.Call{ID: "c1", Args: json.RawMessage(`{"question":"ship it?","options":["yes","","no"]}`)})
	if err != nil || r.Error != "" || r.Text != "yes" {
		t.Fatalf("ask = %+v, %v", r, err)
	}
	es := hist.all()
	if len(es) != 2 || es[0].Kind != "ask" || es[0].Data["question"] != "ship it?" || es[1].Kind != "ask/answer" || es[1].Data["text"] != "yes" {
		t.Fatalf("history = %+v", es)
	}
	if code.paused != 0 {
		t.Fatalf("a native ask paused codemode %d times", code.paused)
	}
	// Esc cancels the call's context: the ask is released, unanswered.
	a.emit = func(Event) {}
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(50*time.Millisecond, cancel)
	r, _ = tl.Call(ctx, agenttools.Call{ID: "c2", Args: json.RawMessage(`{"question":"still there?"}`)})
	if r.Error != "ask: cancelled with no answer" {
		t.Fatalf("cancelled ask = %+v", r)
	}
	if r, _ := tl.Call(context.Background(), agenttools.Call{Args: json.RawMessage(`{"question":" "}`)}); r.Error != "ask: question is empty" {
		t.Fatalf("empty ask = %+v", r)
	}
}

// Native secret keeps tools.secret's contract: the value goes to the
// keychain, the model reads only the reference, and neither the result
// nor history holds the value.
func TestNativeSecretNeverReturnsTheValue(t *testing.T) {
	_, stored := secretEnv(t)
	reg, a, _, hist := mountNative(t)
	tl, ok := reg.Lookup("secret")
	if !ok || !tl.Blocking {
		t.Fatalf("native secret missing or not blocking: %+v", tl)
	}
	const value = "sk-native-9876543210"
	answerNext(t, a, value)
	r, err := tl.Call(context.Background(), agenttools.Call{ID: "s", Args: json.RawMessage(`{"name":"API_KEY","question":"the API needs it","project":"demo"}`)})
	if err != nil || r.Error != "" {
		t.Fatalf("secret = %+v, %v", r, err)
	}
	if r.Text != "stored API_KEY as keychain:bough/demo/API_KEY; applies to project demo's next command or session" {
		t.Fatalf("text = %q", r.Text)
	}
	if stored["bough/demo/API_KEY"] != value {
		t.Fatalf("keychain = %v", stored)
	}
	if strings.Contains(fmt.Sprint(r), value) || strings.Contains(fmt.Sprint(hist.all()), value) {
		t.Fatal("the value reached the result or history")
	}
	if r, _ := tl.Call(context.Background(), agenttools.Call{Args: json.RawMessage(`{"name":"API_KEY","question":"q"}`)}); r.Error != "secret: pass the project slug" {
		t.Fatalf("no project = %+v", r)
	}
}

// A native ask or secret names its call in the ask entry and event, so
// serve, the row and headless can tell its end from a sibling's: on the
// engine a run_js beside it ends with a result that is not the ask's.
// A code-mode ask names none, and its block's result stays its end.
func TestNativeAskNamesItsCall(t *testing.T) {
	_, _ = secretEnv(t)
	reg, a, _, hist := mountNative(t)
	var evs []Event
	a.emit = func(ev Event) {
		evs = append(evs, ev)
		go a.Answer(ev.ID, "yes")
	}
	ask, _ := reg.Lookup("ask")
	if r, _ := ask.Call(context.Background(), agenttools.Call{ID: "c1", Args: json.RawMessage(`{"question":"ship it?"}`)}); r.Text != "yes" {
		t.Fatalf("ask = %+v", r)
	}
	secret, _ := reg.Lookup("secret")
	if r, _ := secret.Call(context.Background(), agenttools.Call{ID: "c2", Args: json.RawMessage(`{"name":"API_KEY","question":"q","project":"demo"}`)}); r.Error != "" {
		t.Fatalf("secret = %+v", r)
	}
	var calls []any
	for _, e := range hist.all() {
		if e.Kind == "ask" {
			calls = append(calls, e.Data["call"])
		}
	}
	if fmt.Sprint(calls) != "[c1 c2]" || len(evs) != 2 || evs[0].Data["call"] != "c1" || evs[1].Data["call"] != "c2" {
		t.Fatalf("ask entries name calls %v, events %+v; want c1 then c2", calls, evs)
	}
	_, ca, _, chist, _ := mount(t, nil)
	ca.emit = func(ev Event) {
		if ev.Data != nil {
			t.Errorf("a code-mode ask event names a call: %+v", ev)
		}
		go ca.Answer(ev.ID, "ok")
	}
	if _, err := ca.ask("code mode?"); err != nil {
		t.Fatal(err)
	}
	if d := chist.all()[0].Data; d["call"] != nil {
		t.Fatalf("a code-mode ask entry names a call: %v", d)
	}
}

// Two asks from one reply run at once on the engine: the second is not
// put to the user until the first is answered, so the one answer slot
// every UI keeps always belongs to the question on screen.
func TestParallelAsksTakeTurns(t *testing.T) {
	t.Parallel()
	reg, a, _, _ := mountNative(t)
	tl, _ := reg.Lookup("ask")
	asked := make(chan Event, 4)
	a.emit = func(ev Event) { asked <- ev }
	type res struct {
		q, text string
	}
	out := make(chan res, 2)
	for _, q := range []string{"FIRST_Q", "SECOND_Q"} {
		go func() {
			r, _ := tl.Call(context.Background(), agenttools.Call{ID: q, Args: json.RawMessage(fmt.Sprintf(`{"question":%q}`, q))})
			out <- res{q, r.Text}
		}()
	}
	first := <-asked
	select {
	case ev := <-asked:
		t.Fatalf("%q was put to the user while %q was open", ev.Text, first.Text)
	case <-time.After(150 * time.Millisecond):
	}
	if err := a.Answer(first.ID, "ANS_"+first.Text); err != nil {
		t.Fatal(err)
	}
	second := <-asked
	if second.Text == first.Text {
		t.Fatalf("the same question twice: %q", second.Text)
	}
	if err := a.Answer(second.ID, "ANS_"+second.Text); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		r := <-out
		if r.text != "ANS_"+r.q {
			t.Errorf("%s got %q", r.q, r.text)
		}
	}
}
