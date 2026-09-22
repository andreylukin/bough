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
