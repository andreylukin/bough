package ui

// The one-line status bar: the identity on the left — "bough · <model>"
// from the row config, bare "bough" once the llm names its model on
// the right, so the model never shows twice; on the right the flash
// message, or the state — "waiting for you" while an
// ask is pending, the inspector hint — else, when idle, the truth
// about this session: "↑in ↓out · $cost · N% ctx · model" (each part
// only when known: cost when priced, the context percentage when the
// model's window is known, the model when the llm names one) — and
// always "? keys" as the way in to the keymap. A narrow pane drops
// parts from the left (tokens, then the model, then the context) until
// the bar fits; it never wraps. The spinner shows only while a turn is
// in flight AND not blocked on the user. The session file is reachable
// via /sessions, not the bar.

import (
	"github.com/charmbracelet/x/ansi"

	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
)

func (m *model) statusBar(cfg *uiCfg) string {
	th := cfg.theme
	tokens, cost, ctx, mdl := usageParts(cfg)
	left := " " + cfg.status
	if mdl != "" {
		// The right side names the model; a duplicate on the left
		// would crowd the tokens out at 80-100 columns.
		left, _, _ = strings.Cut(left, " · ")
	}
	// Candidate right segments, widest first; the first that fits wins
	// and the bare "? keys" is the floor.
	var cands []string
	switch {
	case m.flash != "":
		cands = []string{m.flash}
	case m.inspecting && m.diving != 0:
		cands = []string{"subagent transcript · esc to close"}
	case m.inspecting:
		cands = []string{"inspecting · " + cfg.keys["history_inspect"] + " to close"}
	case m.pendingAsk != "":
		cands = []string{"waiting for you"}
	case m.focusedSpawn() >= 0:
		cands = []string{"subagent card · enter folds · " + cfg.keys["history_inspect"] + " opens its transcript"}
	case m.scrollCue() != "":
		cands = []string{m.scrollCue()}
	default:
		join := func(parts ...string) string {
			return strings.Join(slices.DeleteFunc(parts, func(s string) bool { return s == "" }), " · ")
		}
		cands = slices.Compact([]string{
			join(tokens, cost, ctx, mdl),
			join(cost, ctx, mdl),
			join(cost, ctx),
			join(cost),
		})
	}
	if m.flash == "" {
		// Narrow pane: the usage/scroll cue goes before "? keys" does.
		cands = append(cands, "")
	}
	var right string
	gap := 0
	for _, c := range cands {
		right = c
		if right != "" {
			right += " · "
		}
		right += "? keys"
		if m.running && m.pendingAsk == "" {
			right = m.spin.View() + " " + m.elapsed() + " · " + right
		}
		right += " "
		gap = m.width - lipgloss.Width(left) - lipgloss.Width(right)
		if gap >= 1 {
			break
		}
	}
	if gap < 1 {
		gap = 1
	}
	// One row, always: a narrow pane truncates rather than wrapping the
	// bar onto a second row and pushing the composer off screen.
	line := ansi.Truncate(left+strings.Repeat(" ", gap)+right, m.width, "…")
	return th["status"].Width(m.width).Render(line)
}

// usageParts renders the idle bar's four facts, "" for each unknown:
// the session's token tally as "↑in ↓out", its cost when priced, the
// last request's share of the model's context window, and the model.
func usageParts(cfg *uiCfg) (tokens, cost, ctx, mdl string) {
	if cfg.usage != nil {
		u := cfg.usage.Usage()
		if u.InputTokens > 0 || u.OutputTokens > 0 {
			tokens = "↑" + tokAbbrev(u.InputTokens) + " ↓" + tokAbbrev(u.OutputTokens)
			if u.Priced {
				cost = costText(u.Cost)
			}
		}
		if cfg.limit != nil && u.LastInputTokens > 0 {
			if limit := cfg.limit.ContextLimit(); limit > 0 {
				ctx = fmt.Sprintf("%d%% ctx", u.LastInputTokens*100/limit)
			}
		}
	}
	if cfg.modeler != nil {
		mdl = cfg.modeler.Model()
	}
	return
}

// costText is a dollar figure to the mill, one more place under a
// cent so a cheap turn is not "$0.000".
func costText(c float64) string {
	if c < 0.01 {
		return fmt.Sprintf("$%.4f", c)
	}
	return fmt.Sprintf("$%.3f", c)
}

// tokAbbrev shortens a token count: 850, 12.3k, 1.1M.
func tokAbbrev(n int) string {
	switch {
	case n >= 1_000_000:
		return fmt.Sprintf("%.1fM", float64(n)/1e6)
	case n >= 1000:
		return fmt.Sprintf("%.1fk", float64(n)/1e3)
	}
	return fmt.Sprint(n)
}

// ctxAbbrev shortens a context window without rounding it away:
// 200k, 1M, 1.05M.
func ctxAbbrev(n int) string {
	trim := func(f float64) string {
		return strings.TrimRight(strings.TrimRight(fmt.Sprintf("%.2f", f), "0"), ".")
	}
	switch {
	case n >= 1_000_000:
		return trim(float64(n)/1e6) + "M"
	case n >= 1000:
		return trim(float64(n)/1e3) + "k"
	}
	return fmt.Sprint(n)
}

// elapsed is the in-flight turn's age, whole seconds ("12s", "2m05s"),
// redrawn on every spinner tick.
func (m *model) elapsed() string {
	if m.turnStart.IsZero() {
		return "0s"
	}
	d := time.Since(m.turnStart).Round(time.Second)
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
}
