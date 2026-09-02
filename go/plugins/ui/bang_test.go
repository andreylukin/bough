package ui

// "!" bash mode: a bang line runs as a shell command, renders as an
// echo + a labeled collapsible result block, records "command"/"system"
// history entries, and never reaches the loop/LLM.

import (
	"strings"
	"testing"
)

func TestBangRunsShellAndRendersLabeledBlock(t *testing.T) {
	t.Parallel()
	fl := &fakeLog{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.hlog = fl
	d := newDrv(t, 80, 24, cfg)
	d.typeStr("!echo hi-bang")
	d.press(keyEnter())

	// Never an LLM turn.
	if len(d.sent) != 0 {
		t.Fatalf("a ! line must never reach the loop, sent=%v", d.sent)
	}
	// Echo + labeled result block.
	if len(d.m.blocks) != 2 {
		t.Fatalf("want 2 blocks (command echo + result), got %+v", d.m.blocks)
	}
	if b := d.m.blocks[0]; b.kind != "command" || b.text != "!echo hi-bang" {
		t.Fatalf("echo block = %+v", b)
	}
	b := d.m.blocks[1]
	if b.kind != "result" || b.label != "! echo hi-bang" || b.text != "hi-bang" {
		t.Fatalf("result block = %+v, want kind result, label \"! echo hi-bang\", text hi-bang", b)
	}
	if b.collapsed {
		t.Fatal("bang output the user asked for must start expanded")
	}
	if !b.collapsible() {
		t.Fatal("bang block must stay collapsible")
	}
	if p := d.plain(); !strings.Contains(p, "! echo hi-bang") || !strings.Contains(p, "hi-bang") {
		t.Fatalf("frame missing bang label/output:\n%s", p)
	}
	// History: command + system, never input.
	eq(t, fl.kinds, []string{"command", "system"}, "bang records command + system")
	eq(t, fl.texts, []string{"!echo hi-bang", "hi-bang"}, "recorded texts")
}

func TestBangFailureIsLoud(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("!exit 3")
	d.press(keyEnter())
	if len(d.m.blocks) != 2 {
		t.Fatalf("want 2 blocks, got %+v", d.m.blocks)
	}
	if got := d.m.blocks[1].text; !strings.Contains(got, "exit status 3") {
		t.Fatalf("failure output = %q, want the exit status", got)
	}
}

func TestBangNoOutput(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("!true")
	d.press(keyEnter())
	if got := d.m.blocks[1].text; got != "(no output)" {
		t.Fatalf("silent command output = %q, want (no output)", got)
	}
}
