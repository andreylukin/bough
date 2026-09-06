package ui

// The model picker: bare /model opens it over the action's choices
// with the current row marked and selected; esc goes back untouched;
// enter on another row dispatches "/model <choice>".

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"fmt"
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

// bigModelDrv opens the picker over a catalogue big enough that
// scrolling is not an answer — which is the real shape: OpenRouter
// alone ships 361 models.
func bigModelDrv(t *testing.T) *drv {
	t.Helper()
	choices := []string{"llm-anthropic claude-sonnet-5"}
	for i := range 60 {
		choices = append(choices, fmt.Sprintf("llm-openrouter vendor/model-%02d", i))
	}
	choices = append(choices,
		"llm-openrouter openai/gpt-6-astra",
		"llm-openrouter openai/gpt-6-astra-pro",
		"llm-openai gpt-6-astra")

	r := commands.NewRegistry()
	if err := r.Register(commands.CommandInfo{Name: "model", Usage: "", Summary: "pick"},
		func(args string) (string, error) {
			if args == "" {
				return "", commands.ModelPickerAction("", "llm-anthropic claude-sonnet-5", choices)
			}
			return "model: " + args, nil
		}); err != nil {
		t.Fatal(err)
	}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = r
	d := newDrv(t, 90, 24, cfg)
	d.dispatchLine("/model")
	return d
}

// Typing in the pane searches the WHOLE catalogue. Before this the
// picker showed a dozen models per provider, so a model that existed
// looked like one bough did not have.
func TestModelPickerTypeToSearch(t *testing.T) {
	d := bigModelDrv(t)
	if got := len(d.m.mp.all); got != 64 {
		t.Fatalf("the picker should hold every model, got %d", got)
	}
	d.typeStr("astra")
	if got := len(d.m.mp.rows); got != 3 {
		t.Fatalf("astra should leave 3 rows, got %d: %v", got, d.m.mp.rows)
	}
	p := d.plain()
	for _, want := range []string{"search astra", "3 of 64", "gpt-6-astra"} {
		if !strings.Contains(p, want) {
			t.Errorf("the pane should show %q:\n%s", want, p)
		}
	}
}

// Every word must match, so a provider and a name together narrow.
func TestModelPickerSearchIsAllWords(t *testing.T) {
	d := bigModelDrv(t)
	d.typeStr("openrouter astra")
	if got := len(d.m.mp.rows); got != 2 {
		t.Fatalf("both words should apply, got %d: %v", got, d.m.mp.rows)
	}
	for _, r := range d.m.mp.rows {
		if !strings.Contains(r, "openrouter") || !strings.Contains(r, "astra") {
			t.Errorf("row %q does not match both words", r)
		}
	}
}

// Backspace widens it again; ctrl+u clears it.
func TestModelPickerSearchBackspaceAndClear(t *testing.T) {
	d := bigModelDrv(t)
	d.typeStr("astra")
	d.press(tea.KeyPressMsg{Code: tea.KeyBackspace})
	if d.m.mp.query != "astr" {
		t.Fatalf("backspace should drop one rune, got %q", d.m.mp.query)
	}
	d.press(keyCtrl('u'))
	if d.m.mp.query != "" || len(d.m.mp.rows) != len(d.m.mp.all) {
		t.Errorf("ctrl+u should clear the search, got %q with %d rows", d.m.mp.query, len(d.m.mp.rows))
	}
}

// Enter switches to the row the search selected, not to whatever was
// under the cursor before it.
func TestModelPickerEnterAfterSearch(t *testing.T) {
	d := bigModelDrv(t)
	d.typeStr("astra")
	d.press(keyDown()) // the openrouter one
	d.press(keyEnter())
	if p := d.plain(); !strings.Contains(p, "openai/gpt-6-astra") {
		t.Errorf("enter should switch to the searched model:\n%s", p)
	}
}

// A search with no hits says so instead of showing an empty box.
func TestModelPickerSearchNoMatches(t *testing.T) {
	d := bigModelDrv(t)
	d.typeStr("zzzz")
	if p := d.plain(); !strings.Contains(p, "nothing matches") {
		t.Errorf("an empty result should say so:\n%s", p)
	}
}

// The unfiltered list does not fit, so it is a window with counts.
func TestModelPickerWindowsALongList(t *testing.T) {
	d := bigModelDrv(t)
	p := d.plain()
	if !strings.Contains(p, "64 of 64") {
		t.Errorf("the count should show the whole catalogue:\n%s", p)
	}
	if !strings.Contains(p, "more below") {
		t.Errorf("a list longer than the pane should say what is off it:\n%s", p)
	}
}
