package ui

import (
	"fmt"
	"strconv"
	"strings"

	"charm.land/lipgloss/v2"
)

// Theme tokens and keymap actions are the semantic vocabulary of the
// UI: the "theme" and "keymap" services (map[string]string) override
// entries; unknown tokens/actions fail the mount loudly (Maki-style:
// typos fail loud).

// theme is the parsed token -> style table.
type theme map[string]lipgloss.Style

var themeTokens = []string{
	"user", "assistant", "code", "result", "error", "accent", "dim", "border", "status", "focus",
}

// defaultTheme is restrained and dark-friendly: one cyan accent,
// ANSI palette colors so the user's terminal scheme shows through.
func defaultTheme() theme {
	spec := map[string]string{
		"user":      "5:bold",
		"assistant": "",
		"code":      "252:faint",
		"result":    "",
		"error":     "1",
		"accent":    "6",
		"dim":       "8",
		"border":    "8",
		"status":    "250:236",
		"focus":     "6:bold",
	}
	t := theme{}
	for k, v := range spec {
		st, err := parseStyle(v)
		if err != nil { // unreachable: specs above are valid
			panic(err)
		}
		t[k] = st
	}
	return t
}

// apply overlays a "theme" service map onto t. Unknown tokens or
// unparseable style specs are errors.
func (t theme) apply(m map[string]string) error {
	for tok, spec := range m {
		if _, ok := t[tok]; !ok {
			return fmt.Errorf("ui: theme: unknown token %q (have %s)", tok, strings.Join(themeTokens, ", "))
		}
		st, err := parseStyle(spec)
		if err != nil {
			return fmt.Errorf("ui: theme: token %q: %w", tok, err)
		}
		t[tok] = st
	}
	return nil
}

// parseStyle parses "fg[:bg][:bold|italic|faint]" (hex "#rrggbb" or
// ANSI-256 numbers; empty spec = unstyled). Segment order after fg:
// the first remaining color is bg, everything else must be an attr.
func parseStyle(spec string) (lipgloss.Style, error) {
	st := lipgloss.NewStyle()
	if strings.TrimSpace(spec) == "" {
		return st, nil
	}
	fgSet, bgSet := false, false
	for _, seg := range strings.Split(spec, ":") {
		seg = strings.TrimSpace(seg)
		switch seg {
		case "bold":
			st = st.Bold(true)
			continue
		case "italic":
			st = st.Italic(true)
			continue
		case "faint":
			st = st.Faint(true)
			continue
		}
		if !isColor(seg) {
			return st, fmt.Errorf("bad style segment %q (want color or bold|italic|faint)", seg)
		}
		switch {
		case !fgSet:
			st = st.Foreground(lipgloss.Color(seg))
			fgSet = true
		case !bgSet:
			st = st.Background(lipgloss.Color(seg))
			bgSet = true
		default:
			return st, fmt.Errorf("extra color %q (fg and bg already set)", seg)
		}
	}
	return st, nil
}

func isColor(s string) bool {
	if strings.HasPrefix(s, "#") && (len(s) == 7 || len(s) == 4) {
		_, err := strconv.ParseUint(s[1:], 16, 32)
		return err == nil
	}
	n, err := strconv.Atoi(s)
	return err == nil && n >= 0 && n <= 255
}

// defaultKeymap maps action -> bubbletea key name. The "keymap"
// service overrides entries; unknown actions fail the mount.
//
// Note: the contract's suggested "q" quit default is not used (the
// composer is always focused, so a printable quit key would swallow
// typed text), and its suggested ctrl+h for history_inspect is
// backspace (0x08) in legacy terminals, so ctrl+o is the default.
func defaultKeymap() map[string]string {
	return map[string]string{
		"quit":            "ctrl+c",
		"scroll_up":       "up",
		"scroll_down":     "down",
		"page_up":         "pgup",
		"page_down":       "pgdown",
		"history_inspect": "ctrl+o",
		"block_next":      "tab",
		"block_prev":      "shift+tab",
		"collapse_toggle": "enter", // toggles the focused block; submits otherwise
		"collapse_all":    "",      // config-only: no default key
		"expand_all":      "",      // config-only: no default key
		"clear_input":     "ctrl+l",
	}
}

func applyKeymap(keys map[string]string, m map[string]string) error {
	for action, key := range m {
		if _, ok := keys[action]; !ok {
			known := make([]string, 0, len(keys))
			for a := range keys {
				known = append(known, a)
			}
			return fmt.Errorf("ui: keymap: unknown action %q (have %s)", action, strings.Join(known, ", "))
		}
		if strings.TrimSpace(key) == "" {
			return fmt.Errorf("ui: keymap: empty key for action %q", action)
		}
		keys[action] = key
	}
	return nil
}
