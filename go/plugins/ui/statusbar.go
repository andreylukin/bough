package ui

// The one-line status bar: the identity on the left — "bough · <model>"
// from the row config, bare "bough" once the llm names its model on
// the right, so the model never shows twice; on the right the flash
// message, or the state — "waiting for you" while an
// ask is pending, the inspector hint — else, when idle, the truth
// about this session: "repo (branch) · ↑in ↓out · $cost · N% ctx · ⚡ cache hot · model"
// (each part only when known: cost when priced, the context percentage
// when the model's window is known, the cache chip once the provider
// reports cache tokens (cache.go), the model when the llm names one) — and
// always "? keys" as the way in to the keymap. A narrow pane drops
// parts from the left (tokens, then the model, then the context) until
// the bar fits; it never wraps. The spinner shows only while a turn is
// in flight AND not blocked on the user. The session file is reachable
// via /sessions, not the bar.

import (
	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"fmt"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
)

func (m *model) statusBar(cfg *uiCfg) string {
	th := cfg.theme
	tokens, cost, ctx, mdl := usageParts(cfg)
	left := " " + cfg.status
	switch {
	case m.running && m.activity != "":
		// While it works, the bottom line says what it is doing — the
		// one thing a transcript of collapsed rows cannot show.
		left = " ▸ " + m.activity
	case m.title != "":
		// Once the session has a name it is more use than the app's:
		// several bough windows are told apart by what they are doing.
		left = " " + m.title
	}
	// A transient notice (the flash, the scroll cue) takes the left
	// side, so the usage chips on the right stay in view.
	notice := m.flash
	if notice == "" {
		notice = m.scrollCue()
	}
	if notice != "" {
		left = " " + notice
	} else if mdl != "" {
		// The right side names the model; a duplicate on the left
		// would crowd the tokens out at 80-100 columns.
		left, _, _ = strings.Cut(left, " · ")
	}
	// Candidate right segments, widest first; the first that fits wins
	// and the bare "? keys" is the floor.
	var cands []string
	switch {
	case m.inspecting && m.diving != 0:
		cands = []string{"subagent transcript · esc to close"}
	case m.inspecting:
		cands = []string{"inspecting · " + cfg.keys["history_inspect"] + " to close"}
	case m.pendingAsk != "":
		cands = []string{"waiting for you"}
	case m.focusedSpawn() >= 0:
		cands = []string{"subagent card · enter folds · " + cfg.keys["history_inspect"] + " opens its transcript"}
	case m.suggestion() != "":
		// The small model's guess at the rest of what you are typing;
		// tab takes it. Shown here rather than as ghost text in the
		// composer, where it would hide the draft's real end.
		cands = []string{"↹ …" + strings.TrimSpace(m.suggestion())}
	default:
		join := func(parts ...string) string {
			return strings.Join(slices.DeleteFunc(parts, func(s string) bool { return s == "" }), " · ")
		}
		think := thinkChip(cfg)
		cache := m.cacheChip(cfg)
		mic := ""
		if m.v.mode != "" {
			mic = "🎤 space"
		}
		orb := ""
		if cfg.orb != nil {
			orb = cfg.orb.Line()
		}
		cands = slices.Compact([]string{
			join(orb, m.where, tokens, cost, ctx, cache, think, mdl, mic),
			join(orb, cost, ctx, cache, think, mdl, mic),
			join(m.where, tokens, cost, ctx, cache, think, mdl, mic),
			join(m.where, cost, ctx, cache, think, mdl, mic),
			join(tokens, cost, ctx, cache, think, mdl, mic),
			join(cost, ctx, cache, think, mdl, mic),
			join(cost, ctx, cache, mdl),
			join(cost, ctx, mdl),
			join(cost, ctx),
			join(cost),
		})
		if notice != "" && cost+ctx != "" {
			// Beside a notice the notice is shortened, never cost or ctx.
			cands = slices.DeleteFunc(cands, func(c string) bool {
				return !strings.Contains(c, cost) || !strings.Contains(c, ctx)
			})
		}
	}
	if notice == "" {
		// Narrow pane: the usage goes before "? keys" does; beside a
		// notice the notice is shortened instead.
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
		// Even the bare floor does not fit beside the identity: the
		// identity gives way (shortened, else dropped), never "? keys".
		room := m.width - lipgloss.Width(right) - 1
		switch {
		case room >= 2:
			left = ansi.Truncate(left, room, "…")
			gap = 1
		default:
			left, gap = "", max(0, m.width-lipgloss.Width(right))
		}
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
				if u.LastInputTokens*100 < limit {
					ctx = "<1% ctx"
				}
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

// repoLabel names where the session works as "repo (branch)" — the
// checkout's directory name and its branch, "repo" on a detached head
// — or "" when dir is "" or not in a git checkout.
// whereMsg carries a fresh repoLabel back to Update.
type whereMsg string

// refreshWhere reads the label off the UI goroutine: at start and after
// each turn (which may have switched branch), never per replayed entry.
func (m *model) refreshWhere() tea.Cmd {
	dir := m.cfg.Load().cwd
	return func() tea.Msg { return whereMsg(repoLabel(dir)) }
}

func repoLabel(dir string) string {
	if dir == "" {
		return ""
	}
	top, err := exec.Command("git", "-C", dir, "rev-parse", "--show-toplevel").Output()
	if err != nil {
		return ""
	}
	label := filepath.Base(strings.TrimSpace(string(top)))
	if b, err := exec.Command("git", "-C", dir, "symbolic-ref", "--short", "-q", "HEAD").Output(); err == nil {
		if br := strings.TrimSpace(string(b)); br != "" {
			label += " (" + br + ")"
		}
	}
	return label
}

// elapsed is the in-flight turn's age, whole seconds ("12s", "2m05s"),
// redrawn on every spinner tick.
func (m *model) elapsed() string {
	if m.turnStart.IsZero() {
		return "0s"
	}
	return durText(time.Since(m.turnStart))
}

// durText is a duration in whole seconds: "12s", "2m05s".
func durText(d time.Duration) string {
	d = d.Round(time.Second)
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
}
