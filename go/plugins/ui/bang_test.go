package ui

// "!" bash mode: a bang line runs as a shell command, renders as an
// echo + a labeled collapsible result block, records "command"/"system"
// history entries, and never reaches the loop/LLM.

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

// A pasted multi-line command with control bytes in it: the Shell
// header shows the first line only, cleaned, and no control byte
// reaches the frame. The paste is TestPropFrameInvariants' shrunk
// failure verbatim: the header carried the whole command, wrapped, and
// its width clip left a raw \x7f on the row under it.
func TestBangHeaderKeepsTheCommandToOneCleanLine(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 21, 5, cfgWith(t, nil, nil, histWith("/tmp/h.jsonl", "one", "two")))
	d.typeStr("!")
	d.feed(tea.PasteMsg{Content: "```js\n\x7f\n\n\x1b[31mred\x1b[0m\x1b]8;;http://x\alink\x1b]8;;\a\n\n\x00 \x1b[31mred\x1b[0m\n\n6a62411102b918e218704951119a207953e61d7f0cfc110911736a8a099820d1f7dbf8ee330c7741\ny z13-K. 日本語 v 6712a66510a046f8350001100023f16810010e7b21001dc603331a7d311400992d401ac851a50bb5"})
	next, _ := d.m.Update(keyEnter()) // the shell itself is not run
	d.m = next.(model)
	d.feed(keyTab())
	p := ansi.Strip(d.view())
	if !strings.Contains(p, "Shell · ```js") {
		t.Errorf("header should name the command's first line, cleaned:\n%s", p)
	}
	if strings.ContainsAny(p, "\x7f\x1b") {
		t.Errorf("control bytes reached the frame:\n%q", p)
	}
}

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
	if p := d.plain(); !strings.Contains(p, "Shell · echo hi-bang · exit 0 · 1 line") || !strings.Contains(p, "hi-bang") {
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
