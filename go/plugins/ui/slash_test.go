package ui

// Slash-command dispatch: what reaches the transcript and history.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
)

// A command marked Secret is echoed and recorded without its
// arguments: /connect takes an API key, and the transcript is on
// screen while the history file is on disk.
func TestSecretCommandArgsAreRedacted(t *testing.T) {
	t.Parallel()
	const key = "sk-or-v1-SECRETVALUE"
	r := commands.NewRegistry()
	if err := r.Register(
		commands.CommandInfo{Name: "connect", Usage: "[provider [key]]", Summary: "add a key", Secret: true},
		func(string) (string, error) { return "wrote OPENROUTER_API_KEY to ~/.bough/env", nil },
	); err != nil {
		t.Fatal(err)
	}
	h := &recordingHist{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = r
	cfg.hlog = h
	d := newDrv(t, 100, 24, cfg)

	d.typeStr("/connect openrouter " + key)
	d.press(keyEnter())

	if p := d.plain(); strings.Contains(p, key) {
		t.Errorf("the key must not reach the transcript:\n%s", p)
	}
	if !strings.Contains(d.plain(), "/connect ••••") {
		t.Errorf("the redacted echo should say a command ran:\n%s", d.plain())
	}
	for _, e := range h.appended {
		if strings.Contains(e, key) {
			t.Errorf("the key must not reach history: %q", e)
		}
	}
	// A command with no arguments has nothing to hide.
	d2 := newDrv(t, 100, 24, cfg)
	d2.typeStr("/connect")
	d2.press(keyEnter())
	if !strings.Contains(d2.plain(), "/connect") {
		t.Errorf("an argument-less secret command echoes normally:\n%s", d2.plain())
	}
}

// recordingHist keeps every appended entry's text, so a test can assert
// what did NOT reach history.
type recordingHist struct{ appended []string }

func (r *recordingHist) Append(kind string, data map[string]any) history.Entry {
	t, _ := data["text"].(string)
	r.appended = append(r.appended, kind+": "+t)
	return history.Entry{Kind: kind, Data: data}
}
