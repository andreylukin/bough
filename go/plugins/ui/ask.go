package ui

// Ask blocks: rendering and answer routing for the ask plugin's "ask"
// events (tools.ask blocking on the user). While an ask is pending the
// composer routes the next submission as its answer (a number picks
// that option, anything else is freeform, esc declines) and clicking
// an option row answers with it; an answered ask collapses to a
// one-liner, an unanswered one expires when the turn ends.

import (
	"fmt"
	"strconv"
	"strings"

	xansi "github.com/charmbracelet/x/ansi"
)

// askPlaceholder replaces the composer placeholder while an ask is
// pending; the status bar says "waiting for you" and the spinner
// stops (see statusbar.go).
const askPlaceholder = "type a number or your answer · esc to skip"

// renderAsk renders one ask block: pending = accent question over
// numbered option rows word-wrapped to the pane with continuation
// lines indented under the option text (b.optRows records which option
// each line belongs to, for hit-testing in handleClick); resolved = the "❯? question →
// answer" one-liner, with "(expired)" for an ask that never got one.
func (m *model) renderAsk(b *block, th theme) string {
	if b.answered {
		return th["dim"].Render("❯? ") + b.text + th["dim"].Render(" → ") + b.answer
	}
	if b.expired {
		return th["dim"].Render("❯? " + b.text + " → (expired)")
	}
	w := max(m.width, 10)
	var lines []string
	b.optRows = b.optRows[:0]
	for _, l := range strings.Split(xansi.Wrap("? "+b.text, w, ""), "\n") {
		lines = append(lines, th["accent"].Render(l))
		b.optRows = append(b.optRows, -1)
	}
	for i, o := range b.options {
		num := fmt.Sprintf("  %d.", i+1)
		pad := len(num) + 1
		for j, l := range strings.Split(xansi.Wrap(o, max(w-pad, 1), ""), "\n") {
			if j == 0 {
				lines = append(lines, th["dim"].Render(num)+" "+l)
			} else {
				lines = append(lines, strings.Repeat(" ", pad)+l)
			}
			b.optRows = append(b.optRows, i)
		}
	}
	return strings.Join(lines, "\n")
}

// pendingBlock finds the ask block the composer is routed to.
func (m *model) pendingBlock() *block {
	if m.pendingAsk == "" {
		return nil
	}
	for i := len(m.blocks) - 1; i >= 0; i-- {
		b := &m.blocks[i]
		if b.kind == "ask" && b.askID == m.pendingAsk && !b.answered && !b.expired {
			return b
		}
	}
	return nil
}

// answerPending routes one composer submission to the pending ask,
// reporting whether it consumed the submission. A bare number in the
// options' range picks that option; anything else is the literal
// answer.
func (m *model) answerPending(text string) bool {
	b := m.pendingBlock()
	if b == nil {
		m.clearPendingAsk() // stale routing (e.g. /clear dropped the block)
		return false
	}
	if n, err := strconv.Atoi(strings.TrimSpace(text)); err == nil && n >= 1 && n <= len(b.options) {
		text = b.options[n-1]
	}
	m.input.Reset()
	m.syncPalette()
	m.answerAsk(b, text)
	return true
}

// answerAsk resolves one ask block through the "ask-answers" service.
// A missing service or a refused answer (the ask already timed out)
// expires the block loudly instead of leaving the composer captured.
func (m *model) answerAsk(b *block, text string) {
	if ask := m.cfg.Load().ask; ask == nil {
		b.expired = true
		m.flash = "no ask-answers service mounted"
	} else if err := ask.Answer(b.askID, text); err != nil {
		b.expired = true
		m.flash = err.Error()
	} else {
		b.answered, b.answer = true, text
	}
	if m.pendingAsk == b.askID {
		m.clearPendingAsk()
	}
	m.refresh()
	m.vp.GotoBottom()
}

// expireAsks marks every unanswered ask block expired and releases the
// composer: the turn ended (done, or the ask timed out into a run
// error) with no answer coming.
func (m *model) expireAsks() {
	for i := range m.blocks {
		if b := &m.blocks[i]; b.kind == "ask" && !b.answered {
			b.expired = true
		}
	}
	if m.pendingAsk != "" {
		m.clearPendingAsk()
	}
}

func (m *model) clearPendingAsk() {
	m.pendingAsk = ""
	m.input.Placeholder = "say something"
	if m.input.Value() == "" {
		m.input.SetValue(m.askStash)
	}
	m.askStash = ""
}

// strList tolerates both the in-process ([]string) and JSONL-replayed
// ([]any) shapes of an ask entry's options.
func strList(v any) []string {
	switch l := v.(type) {
	case []string:
		return l
	case []any:
		out := make([]string, 0, len(l))
		for _, x := range l {
			if s, ok := x.(string); ok {
				out = append(out, s)
			}
		}
		return out
	}
	return nil
}
