package ui

// The model picker: bare /model opens it over the action's choices
// with the current row marked and selected; esc goes back untouched;
// enter on another row dispatches "/model <choice>".

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/commands"
)

func modelDrv(t *testing.T) (*drv, *[]string) {
	t.Helper()
	var ran []string
	r := commands.NewRegistry()
	err := r.Register(commands.CommandInfo{Name: "model", Usage: "", Summary: "pick"},
		func(args string) (string, error) {
			ran = append(ran, args)
			if args == "" {
				return "", commands.ModelPickerAction("", "llm-anthropic claude-sonnet-5",
					[]string{"llm-anthropic claude-sonnet-5", "llm-cerebras gpt-oss-120b", "llm-echo"})
			}
			return "model: " + args, nil
		})
	if err != nil {
		t.Fatal(err)
	}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = r
	d := newDrv(t, 80, 24, cfg)
	return d, &ran
}

func TestSlashModelOpensPickerCurrentMarked(t *testing.T) {
	d, _ := modelDrv(t)
	d.dispatchLine("/model")
	if !d.m.mp.open || d.m.mp.pick != 0 {
		t.Fatalf("/model should open the picker on the current row: %+v", d.m.mp)
	}
	p := d.plain()
	for _, want := range []string{"pick a model", "▸ llm-anthropic claude-sonnet-5 (current)", "  llm-cerebras gpt-oss-120b", "  llm-echo", "esc back"} {
		if !strings.Contains(p, want) {
			t.Errorf("picker missing %q:\n%s", want, p)
		}
	}
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if d.m.mp.open {
		t.Fatal("esc should close the picker")
	}
}

func TestModelPickerEnterDispatchesChoice(t *testing.T) {
	d, ran := modelDrv(t)
	d.dispatchLine("/model")
	d.press(keyDown())
	d.press(keyEnter())
	if d.m.mp.open {
		t.Fatal("enter should close the picker")
	}
	if len(*ran) != 2 || (*ran)[1] != "llm-cerebras gpt-oss-120b" {
		t.Fatalf("enter should run /model with the choice, ran %v", *ran)
	}
	if p := d.plain(); !strings.Contains(p, "model: llm-cerebras gpt-oss-120b") {
		t.Fatalf("swap result should land in the transcript:\n%s", p)
	}
}

func TestModelPickerEnterOnCurrentIsNoop(t *testing.T) {
	d, ran := modelDrv(t)
	d.dispatchLine("/model")
	d.press(keyEnter())
	if d.m.mp.open || len(*ran) != 1 {
		t.Fatalf("enter on the current row should just close: open=%v ran=%v", d.m.mp.open, *ran)
	}
}
