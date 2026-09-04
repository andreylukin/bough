package ui

// Thinking level: shift+tab at rest cycles how hard the model reasons,
// the way tab-style mode cycling works elsewhere. The provider decides
// what a level means; a provider that cannot reason says so instead of
// pretending. The reasoning itself, when the provider streams it,
// arrives as "thinking-delta" events and renders as one collapsed
// block above the reply (see blocks.go).

import (
	"strings"

	"github.com/andreylukin/bough/plugins/llm"
)

// thinkLevels is the cycle order: the provider's own default, then off,
// then harder and harder.
var thinkLevels = append([]string{""}, llm.Efforts...)

// cycleThinking advances to the next level and returns the status-bar
// note (the ui never errors on a keypress).
func (m *model) cycleThinking() string {
	e := m.cfg.Load().effort
	if e == nil {
		return "this provider has no thinking level to set"
	}
	cur := e.Effort()
	next := thinkLevels[0]
	for i, l := range thinkLevels {
		if l == cur {
			next = thinkLevels[(i+1)%len(thinkLevels)]
			break
		}
	}
	if err := e.SetEffort(next); err != nil {
		return "thinking: " + err.Error()
	}
	return "thinking: " + thinkLabel(next) + " (from the next message)"
}

// thinkLabel names a level for a reader; "" is the provider's default.
func thinkLabel(level string) string {
	if level == "" {
		return "default"
	}
	return level
}

// thinkChip is the status-bar chip: shown only when the level has been
// changed from the provider's default, so an untouched session keeps
// its bar clean.
func thinkChip(cfg *uiCfg) string {
	if cfg.effort == nil {
		return ""
	}
	if l := cfg.effort.Effort(); l != "" {
		return "think " + strings.ToLower(l)
	}
	return ""
}
