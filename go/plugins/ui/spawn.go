package ui

// Subagent cards: one "spawn" block per tools.spawn call, updated in
// place by the child's sub:* events — status glyph, call count, the
// last call's label, and on completion the child's report. The child's
// full transcript is kept per card and opened in the overlay pane
// (ctrl+o on the focused card), never interleaved with the parent's
// story.

import (
	"fmt"
	"strings"
	"time"

	"github.com/charmbracelet/lipgloss"
	"github.com/charmbracelet/x/ansi"
)

// line is one sanitized line of s, cut to n runes.
func line(s string, n int) string {
	s = strings.SplitN(strings.TrimSpace(sanitizeText(s)), "\n", 2)[0]
	if r := []rune(s); len(r) > n {
		return string(r[:n]) + "…"
	}
	return s
}

// subState is a spawn card's live state.
type subState struct {
	worker  int
	status  string // "running", "ok", "failed", "error"
	calls   int
	last    string // label of the last code event ("Ran: ls")
	steps   int
	errText string
	started time.Time
	elapsed time.Duration
	log     []Event // the child's transcript, in order
}

// workerOf reads the event's worker number (0 when absent).
func workerOf(ev Event) int {
	switch n := ev.Data["worker"].(type) {
	case int:
		return n
	case int64:
		return int(n)
	case float64:
		return int(n)
	}
	return 0
}

// spawnCard finds the card for a worker, creating one (task unknown)
// for histories written before sub:start existed.
func (m *model) spawnCard(worker int) *block {
	for i := len(m.blocks) - 1; i >= 0; i-- {
		if b := &m.blocks[i]; b.kind == "spawn" && b.sub != nil && b.sub.worker == worker {
			return b
		}
	}
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "spawn", collapsed: m.cfg.Load().collapse != "none",
		sub: &subState{worker: worker, status: "running", started: time.Now()}})
	m.nextID++
	return &m.blocks[len(m.blocks)-1]
}

// addSubEvent folds one sub:* event into its worker's card.
func (m *model) addSubEvent(ev Event) {
	kind := strings.TrimPrefix(ev.Kind, "sub:")
	worker := workerOf(ev)
	var b *block
	if kind == "start" {
		b = m.spawnCard(worker)
		b.label = ev.Text // the task
	} else {
		b = m.spawnCard(worker)
	}
	s := b.sub
	if kind != "start" && kind != "done" {
		s.log = append(s.log, Event{Kind: kind, Text: strings.TrimRight(ev.Text, "\n")})
	}
	switch kind {
	case "code":
		s.calls++
		s.last = codeLabel(ev.Text)
	case "assistant":
		b.text = strings.TrimSpace(ev.Text) // the latest reply is the report
	case "error":
		s.errText = strings.SplitN(strings.TrimSpace(ev.Text), "\n", 2)[0]
		s.status = "error"
	case "done":
		s.elapsed = time.Since(s.started)
		if st, _ := ev.Data["status"].(string); st != "" {
			s.status = st
		} else if s.status == "running" {
			s.status = "ok"
		}
		switch n := ev.Data["steps"].(type) {
		case int:
			s.steps = n
		case float64:
			s.steps = int(n)
		}
	}
	if m.diving == b.id && m.inspecting {
		m.refreshOverlay()
	}
	m.refresh()
}

// renderSpawn is the card: one header row, plus the report in a box
// when expanded.
func (m *model) renderSpawn(b *block, th theme) string {
	s := b.sub
	if s == nil { // a bare "spawn" event (no worker state): render as a result
		return m.header(b, th)
	}
	glyph := "▸"
	if !b.collapsed {
		glyph = "▾"
	}
	var mark string
	switch s.status {
	case "running":
		mark = m.spin.View()
	case "ok":
		mark = th["accent"].Render("✔")
	default:
		mark = th["error"].Render("✗")
	}
	task := line(b.label, 60)
	if task == "" {
		task = "task"
	}
	parts := []string{fmt.Sprintf("sub %d · %s", s.worker, task)}
	if s.calls > 0 {
		parts = append(parts, plural(s.calls, "call"))
	}
	switch s.status {
	case "running":
		if s.last != "" {
			parts = append(parts, line(s.last, 60))
		}
	case "ok":
		parts = append(parts, fmtElapsed(s.elapsed))
	case "failed":
		parts = append(parts, "reported failure")
	default:
		if s.errText != "" {
			parts = append(parts, line(s.errText, 80))
		}
	}
	st := th["dim"]
	if b.id == m.focusID {
		st = th["focus"]
	}
	head := glyph + " " + mark + " " + st.Render(strings.Join(parts, " · "))
	if lipgloss.Width(head) > m.width && m.width > 1 {
		head = ansi.Truncate(head, m.width, "…")
	}
	if b.collapsed {
		return head
	}
	body := b.text
	if body == "" {
		if s.status == "running" {
			body = "(working…)"
		} else {
			body = "(no report)"
		}
	}
	return head + "\n" + m.box(body, th["result"], th["border"])
}

func fmtElapsed(d time.Duration) string {
	if d < time.Second {
		return "<1s"
	}
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
}

// focusedSpawn returns the index of the focused spawn card, -1 if the
// focus is elsewhere.
func (m *model) focusedSpawn() int {
	for i := range m.blocks {
		if b := &m.blocks[i]; b.kind == "spawn" && b.id == m.focusID {
			return i
		}
	}
	return -1
}

// subTranscript renders a card's child transcript for the overlay:
// the task, then each child event as the block it would be in a
// transcript of its own.
func (m *model) subTranscript(b *block, cfg *uiCfg) string {
	th := cfg.theme
	s := b.sub
	var sb strings.Builder
	sb.WriteString(th["accent"].Render(fmt.Sprintf("subagent %d", s.worker)) + " " + th["dim"].Render("· "+line(b.label, 200)) + "\n")
	for _, ev := range s.log {
		tmp := block{id: -1, kind: ev.Kind, text: ev.Text}
		if ev.Kind == "result" {
			tmp.text = resultText(ev.Text)
		}
		sb.WriteString("\n" + m.render(&tmp, cfg))
	}
	switch s.status {
	case "running":
		sb.WriteString("\n\n" + th["dim"].Render(m.spin.View()+" working…"))
	case "ok":
		sb.WriteString("\n\n" + th["dim"].Render(fmt.Sprintf("✔ done · %s · %s", plural(s.steps, "step"), fmtElapsed(s.elapsed))))
	default:
		sb.WriteString("\n\n" + th["error"].Render("✗ "+s.status+" · "+line(s.errText, 200)))
	}
	return sb.String()
}
