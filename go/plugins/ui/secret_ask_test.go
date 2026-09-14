package ui

// Secret asks (tools.secret): the composer masks the value, Enter hands
// it to the ask raw, and it never shows up in the view afterwards.

import (
	"bytes"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestSecretAskMasksAndNeverShowsValue(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(eventMsg{Kind: "ask", Text: "Secret DEVPI_URL for demo: tests", ID: "ask-1", Secret: true})
	const value = "/hunter2!x"
	d.typeStr(value)
	if p := d.plain(); strings.Contains(p, value) || !strings.Contains(p, strings.Repeat("•", len([]rune(value)))) {
		t.Fatalf("composer should mask the value:\n%s", p)
	}
	d.press(keyEnter())
	if len(fa.texts) != 1 || fa.texts[0] != value {
		t.Fatalf("answer = %v, want the raw value (a leading / must not dispatch)", fa.texts)
	}
	if p := d.plain(); strings.Contains(p, value) || !strings.Contains(p, "[secret stored]") {
		t.Fatalf("after answering the view must not hold the value:\n%s", p)
	}
	if d.m.input.Value() != "" {
		t.Fatalf("composer kept the value: %q", d.m.input.Value())
	}
	for _, b := range d.m.blocks {
		if strings.Contains(b.text, value) || strings.Contains(b.answer, value) {
			t.Fatalf("block %s holds the value", b.kind)
		}
	}
	// Recall: Up on the empty composer must not bring the value back.
	d.press(keyUp())
	if strings.Contains(d.m.input.Value(), value) {
		t.Fatalf("history recall returned the value")
	}
}

func TestSecretAskExpiredDraftDropped(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(eventMsg{Kind: "ask", Text: "Secret X for demo: why", ID: "ask-1", Secret: true})
	const value = "half-typed-s3cr3t"
	d.typeStr(value)
	d.m.expireAsks() // turn cancelled or timed out before Enter
	if p := d.plain(); strings.Contains(p, value) {
		t.Fatalf("expired secret draft shown:\n%s", p)
	}
	if v := d.m.input.Value(); strings.Contains(v, value) {
		t.Fatalf("composer kept the secret draft: %q", v)
	}
	d.press(keyEnter())
	if len(fa.texts) != 0 || strings.Contains(d.plain(), value) {
		t.Fatalf("value sent after expiry: %v", fa.texts)
	}
}

func TestSecretAskPasteIsRaw(t *testing.T) {
	t.Parallel()
	d, fa := askDrv(t)
	d.feed(eventMsg{Kind: "ask", Text: "Secret X for demo: why", ID: "ask-1", Secret: true})
	value := strings.Repeat("tok3n", 400) // past the paste-collapse threshold
	d.feed(tea.PasteMsg{Content: value})
	d.press(keyEnter())
	if len(fa.texts) != 1 || fa.texts[0] != value {
		t.Fatalf("answer = %.60q, want the raw paste", fa.texts)
	}
}

func TestHeadlessSecretAskRawLineNotPrinted(t *testing.T) {
	var out, errb bytes.Buffer
	oldOut, oldErr, oldJSON := hlOut, hlErr, HeadlessJSON
	hlOut, hlErr, HeadlessJSON = &out, &errb, true
	fa := &fakeAsk{}
	hlMu.Lock()
	oldAns := hlAnswer
	hlAnswer = fa
	hlMu.Unlock()
	defer func() {
		hlOut, hlErr, HeadlessJSON = oldOut, oldErr, oldJSON
		hlMu.Lock()
		hlAnswer, hlAsk = oldAns, nil
		hlMu.Unlock()
	}()

	hlPrint(Event{Kind: "ask", Text: "Secret X for demo: why", ID: "ask-9", Secret: true})
	if !strings.Contains(out.String(), `"secret":true`) {
		t.Fatalf("ask line lacks secret flag: %s", out.String())
	}
	const value = `{"prompt":"s3cr3t"}`
	hlLineIn(value)
	if len(fa.texts) != 1 || fa.texts[0] != value {
		t.Fatalf("answer = %v, want the raw line", fa.texts)
	}
	if strings.Contains(out.String()+errb.String(), "s3cr3t") {
		t.Fatalf("value printed: %s %s", out.String(), errb.String())
	}
}
