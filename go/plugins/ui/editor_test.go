package ui

// ctrl+g external editor (editor.go): the draft round-trips through
// a fake $EDITOR that appends a line.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestExternalEditorRoundTrip(t *testing.T) {
	script := filepath.Join(t.TempDir(), "ed.sh")
	os.WriteFile(script, []byte("#!/bin/sh\nprintf '\\nappended by editor\\n' >> \"$1\"\n"), 0o755)
	t.Setenv("VISUAL", "")
	t.Setenv("EDITOR", script)

	d := defaultDrv(t)
	d.typeStr("first draft")
	// ctrl+g is external_editor by default: it yields the suspending
	// exec cmd rather than touching the draft.
	next, cmd := d.m.Update(keyCtrl('g'))
	d.m = next.(model)
	if cmd == nil || d.m.input.Value() != "first draft" {
		t.Fatalf("ctrl+g should return the editor cmd and keep the draft, got cmd=%v draft=%q", cmd != nil, d.m.input.Value())
	}

	// The same pieces the cmd runs, driven directly: save, edit, finish.
	path, err := saveDraft(d.m.input.Value())
	if err != nil {
		t.Fatal(err)
	}
	if err := editorCommand(path).Run(); err != nil {
		t.Fatalf("fake editor: %v", err)
	}
	d.feed(editorDoneMsg{path: path})
	if got := d.m.input.Value(); got != "first draft\nappended by editor" {
		t.Fatalf("draft after editor = %q", got)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Error("the temp draft file should be removed")
	}
	if d.m.input.Height() != 2 {
		t.Errorf("composer should grow to the edited draft, got %d rows", d.m.input.Height())
	}
}

func TestExternalEditorFailureKeepsDraft(t *testing.T) {
	t.Setenv("VISUAL", "false")
	d := defaultDrv(t)
	d.typeStr("keep me")
	path, _ := saveDraft("keep me")
	err := editorCommand(path).Run()
	if err == nil {
		t.Fatal("`false` should fail")
	}
	d.feed(editorDoneMsg{path: path, err: err})
	if d.m.input.Value() != "keep me" || !strings.Contains(d.m.flash, "draft kept") {
		t.Errorf("draft %q flash %q", d.m.input.Value(), d.m.flash)
	}
}

func TestEditorCommandFallbacks(t *testing.T) {
	t.Setenv("VISUAL", "")
	t.Setenv("EDITOR", "")
	if c := editorCommand("f"); filepath.Base(c.Args[0]) != "nano" || c.Args[1] != "f" {
		t.Errorf("default should be nano, got %v", c.Args)
	}
	t.Setenv("EDITOR", "code --wait")
	if c := editorCommand("f"); c.Args[1] != "--wait" || c.Args[2] != "f" {
		t.Errorf("flags should survive, got %v", c.Args)
	}
}
