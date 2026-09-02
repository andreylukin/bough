package ui

import (
	"strings"
	"sync/atomic"
	"testing"
)

func testModel(t *testing.T) model {
	t.Helper()
	var cfg atomic.Pointer[uiCfg]
	cfg.Store(newCfg(defaultTheme(), defaultKeymap(), "bough", nil))
	return newModel(80, 24, func(string) {}, nil, &cfg)
}

func TestResultCollapse(t *testing.T) {
	m := testModel(t)
	long := strings.TrimSuffix(strings.Repeat("line\n", 20), "\n")
	m.addEvent(Event{Kind: "result", Text: long})
	if !m.blocks[0].collapsed {
		t.Fatal("20-line result should start collapsed")
	}
	out := m.render(&m.blocks[0], m.cfg.Load())
	if !strings.Contains(out, "12 more lines") {
		t.Errorf("collapsed render missing tail hint:\n%s", out)
	}
	if got := strings.Count(out, "line"); got != collapseHead+1 { // +1 for "more lines"
		t.Errorf("collapsed render shows %d lines, want %d", got-1, collapseHead)
	}

	m.blocks[0].collapsed = false
	out = m.render(&m.blocks[0], m.cfg.Load())
	if strings.Count(out, "line") != 20 {
		t.Errorf("expanded render should show all 20 lines:\n%s", out)
	}

	// A short result never collapses, even if toggled.
	m.addEvent(Event{Kind: "result", Text: "just one line"})
	if m.blocks[1].collapsed {
		t.Fatal("1-line result should not start collapsed")
	}
	m.blocks[1].collapsed = true // simulate a stray toggle
	out = m.render(&m.blocks[1], m.cfg.Load())
	if !strings.Contains(out, "just one line") || strings.Contains(out, "more lines") {
		t.Errorf("short collapsed result should render in full:\n%s", out)
	}
}

func TestParseStyle(t *testing.T) {
	good := []string{"", "5", "5:bold", "#ffaf00:bold", "250:236", "#fff:#000:faint", "1:bold:italic"}
	for _, spec := range good {
		if _, err := parseStyle(spec); err != nil {
			t.Errorf("parseStyle(%q): %v", spec, err)
		}
	}
	bad := []string{"reddish", "5:bold:blink", "#ggg", "1:2:3", "300"}
	for _, spec := range bad {
		if _, err := parseStyle(spec); err == nil {
			t.Errorf("parseStyle(%q): want error", spec)
		}
	}
}

func TestThemeAndKeymapValidation(t *testing.T) {
	th := defaultTheme()
	if err := th.apply(map[string]string{"user": "#ffaf00:bold"}); err != nil {
		t.Errorf("valid theme override: %v", err)
	}
	if err := th.apply(map[string]string{"theem": "5"}); err == nil {
		t.Error("unknown theme token should fail loud")
	}
	keys := defaultKeymap()
	if err := applyKeymap(keys, map[string]string{"history_inspect": "ctrl+g"}); err != nil {
		t.Errorf("valid keymap override: %v", err)
	}
	if keys["history_inspect"] != "ctrl+g" {
		t.Error("keymap override not applied")
	}
	if err := applyKeymap(keys, map[string]string{"histry": "ctrl+g"}); err == nil {
		t.Error("unknown keymap action should fail loud")
	}
}
