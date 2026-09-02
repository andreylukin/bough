package ui

// Ask blocks: pending rendering, composer answer routing (numbers,
// freeform, esc), option clicks, collapsed/expired replay.

import (
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

var errTest = errors.New("ask: no pending ask")

// fakeAsk records Answer calls for the "ask-answers" seam.
type fakeAsk struct {
	ids, texts []string
	err        error
}

func (f *fakeAsk) Answer(id, text string) error {
	f.ids = append(f.ids, id)
	f.texts = append(f.texts, text)
	return f.err
}

// askDrv is a driver whose cfg carries a fakeAsk.
func askDrv(t *testing.T) (*drv, *fakeAsk) {
	t.Helper()
	fa := &fakeAsk{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.ask = fa
	return newDrv(t, 80, 24, cfg), fa
}

func askEvent() eventMsg {
	return eventMsg{Kind: "ask", Text: "fav color?", ID: "ask-1", Options: []string{"red", "blue"}}
}

func TestAskRendersQuestionAndOptions(t *testing.T) {
	t.Parallel()
	d, _ := askDrv(t)
	d.feed(askEvent())
	p := d.plain()
	if !strings.Contains(p, "? fav color?") {
		t.Errorf("pending ask missing question:\n%s", p)
	}
	if !strings.Contains(p, "1. red") || !strings.Contains(p, "2. blue") {
		t.Errorf("pending ask missing numbered options:\n%s", p)
	}
	if d.m.pendingAsk != "ask-1" {
		t.Errorf("pendingAsk = %q, want ask-1", d.m.pendingAsk)
	}
	if d.m.input.Placeholder != askPlaceholder {
		t.Errorf("placeholder = %q, want %q", d.m.input.Placeholder, askPlaceholder)
	}
}

// A pending ask must not swallow a "/" or "!" line: those route as
// usual and the ask stays pending.
func TestAskDoesNotSwallowSlashOrBang(t *testing.T) {
	t.Parallel()
	fa := &fakeAsk{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.ask = fa
	cfg.cmds = reg(t, "help")
	d := newDrv(t, 80, 24, cfg)
	d.feed(askEvent())
	d.typeStr("/help")
	d.press(keyEnter())
	if len(fa.ids) != 0 {
		t.Fatalf("a / line must not answer the ask: %v", fa.texts)
	}
	if d.m.pendingAsk != "ask-1" {
		t.Fatal("the ask should still be pending after a / command")
	}
	if p := d.plain(); !strings.Contains(p, "help ran") {
		t.Errorf("/help should have dispatched:\n%s", p)
	}
	d.typeStr("!true")
	d.press(keyEnter())
	if len(fa.ids) != 0 || d.m.pendingAsk != "ask-1" {
		t.Fatalf("a ! line must not answer the ask: %v pending=%q", fa.texts, d.m.pendingAsk)
	}
}

func TestAskComposerFreeformAnswer(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(askEvent())
	d.typeStr("chartreuse")
	d.press(keyEnter())
	if len(fa.ids) != 1 || fa.ids[0] != "ask-1" || fa.texts[0] != "chartreuse" {
		t.Fatalf("answer not routed: ids=%v texts=%v", fa.ids, fa.texts)
	}
	if len(d.sent) != 0 {
		t.Errorf("answer must not reach the loop inputs: %v", d.sent)
	}
	p := d.plain()
	if !strings.Contains(p, "❯? fav color? → chartreuse") {
		t.Errorf("answered ask should collapse to a one-liner:\n%s", p)
	}
	if strings.Contains(p, "1. red") {
		t.Errorf("options should be gone after answering:\n%s", p)
	}
	if d.m.pendingAsk != "" || d.m.input.Placeholder != "say something" {
		t.Errorf("pending state not cleared: %q / %q", d.m.pendingAsk, d.m.input.Placeholder)
	}
}

func TestAskComposerNumberPicksOption(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(askEvent())
	d.typeStr("2")
	d.press(keyEnter())
	if len(fa.texts) != 1 || fa.texts[0] != "blue" {
		t.Fatalf("number should pick the option: %v", fa.texts)
	}
}

func TestAskEscDeclines(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(askEvent())
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if len(fa.texts) != 1 || fa.texts[0] != "(declined)" {
		t.Fatalf("esc should answer (declined): %v", fa.texts)
	}
	if p := d.plain(); !strings.Contains(p, "❯? fav color? → (declined)") {
		t.Errorf("declined ask should show the one-liner:\n%s", p)
	}
}

func TestAskOptionClickAnswers(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(askEvent())
	// The ask block is the only block: question on line 0, option 1 on
	// line 1 of its range.
	var start int
	found := false
	for _, r := range d.m.ranges {
		if d.m.blocks[r.idx].kind == "ask" {
			start, found = r.start, true
		}
	}
	if !found {
		t.Fatal("no ask block range")
	}
	d.feed(tea.MouseClickMsg{X: 0, Y: start + 1 - d.m.vp.YOffset(), Button: tea.MouseLeft})
	d.feed(tea.MouseReleaseMsg{X: 0, Y: start + 1 - d.m.vp.YOffset(), Button: tea.MouseLeft})
	if len(fa.texts) != 1 || fa.texts[0] != "red" {
		t.Fatalf("clicking option row 1 should answer red: %v", fa.texts)
	}
}

func TestAskAnswerErrorExpires(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	fa.err = errTest
	d.feed(askEvent())
	d.typeStr("blue")
	d.press(keyEnter())
	p := d.plain()
	if !strings.Contains(p, "❯? fav color? → (expired)") {
		t.Errorf("refused answer should expire the ask:\n%s", p)
	}
	if d.m.pendingAsk != "" {
		t.Error("refused answer should release the composer")
	}
}

func TestAskExpiresOnDone(t *testing.T) {
	t.Parallel()
	d, _ := askDrv(t)
	d.feed(askEvent())
	d.event("done", "") // e.g. the ask timed out and the turn ended
	p := d.plain()
	if !strings.Contains(p, "❯? fav color? → (expired)") {
		t.Errorf("unanswered ask should expire at turn end:\n%s", p)
	}
	if d.m.pendingAsk != "" || d.m.input.Placeholder != "say something" {
		t.Error("turn end should release the composer")
	}
	// The next submission is a normal input again.
	d.typeStr("hello")
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != "hello" {
		t.Errorf("post-expiry submission should reach the loop: %v", d.sent)
	}
}

func TestAskReplayCollapsed(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/x.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "ask", Data: map[string]any{
			"question": "deploy now?", "options": []any{"yes", "no"}, "id": "ask-1"}},
		{Seq: 2, Kind: "ask/answer", Data: map[string]any{"id": "ask-1", "text": "yes"}},
	}}
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	p := d.plain()
	if !strings.Contains(p, "❯? deploy now? → yes") {
		t.Errorf("replayed answered ask should be the one-liner:\n%s", p)
	}
	if strings.Contains(p, "1. yes") {
		t.Errorf("replayed answered ask should not list options:\n%s", p)
	}
	if d.m.pendingAsk != "" {
		t.Error("replayed answered ask must not capture the composer")
	}
}

func TestAskReplayPendingExpired(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/x.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "ask", Data: map[string]any{
			"question": "deploy now?", "options": []any{"yes"}, "id": "ask-1"}},
	}}
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	p := d.plain()
	if !strings.Contains(p, "❯? deploy now? → (expired)") {
		t.Errorf("replayed unanswered ask should render expired:\n%s", p)
	}
	if d.m.pendingAsk != "" || d.m.input.Placeholder == "(answering)" {
		t.Error("replayed unanswered ask must not capture the composer")
	}
}
