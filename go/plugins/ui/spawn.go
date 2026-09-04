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
	lastOut string // first line of the last result (the heartbeat under last)
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
	// A card follows the collapse policy like every other detail block.
	// Its closed head is a live row on its own — spinner, state, call
	// count, elapsed, and the call running right now — so watching a
	// subagent costs one line, not a panel. Four open panels buried the
	// transcript they were supposed to summarise.
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "spawn",
		collapsed: m.cfg.Load().collapse != "none",
		sub:       &subState{worker: worker, status: "running", started: time.Now()}})
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
		s.lastOut = ""
	case "result":
		s.lastOut = line(ev.Text, 200)
	case "assistant":
		b.text = strings.TrimSpace(ev.Text) // the latest reply is the report
	case "error":
		s.errText = strings.SplitN(strings.TrimSpace(ev.Text), "\n", 2)[0]
		s.status = "error"
	case "done":
		// A replayed history lands in one burst: its cards would all
		// claim "<1s". No real subagent finishes inside a second, so
		// that reads as "unknown" and the header omits it.
		if s.elapsed = time.Since(s.started); s.elapsed < time.Second {
			s.elapsed = 0
		}
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

// reportCap is how many report lines a finished card shows inline;
// the rest is a "+N lines" note (the full transcript is ctrl+o away).
const reportCap = 6

// renderSpawn is the card. Expanded (the default — a subagent is
// something to watch, not a tool row to fold): a status header, the
// whole task under an accent bar, then while running the last call
// and the first line of its output (the child's heartbeat), and once
// done the report, capped at reportCap lines. Collapsed: one line with
// the task cut short. Focus restyles only; the text is the same.
func (m *model) renderSpawn(b *block, th theme) string {
	s := b.sub
	if s == nil { // a bare "spawn" event (no worker state): render as a result
		return m.header(b, th)
	}
	glyph := "▸"
	if !b.collapsed {
		glyph = "▾"
	}
	var mark, state string
	switch s.status {
	case "running":
		mark, state = m.spin.View(), "running"
	case "ok":
		mark, state = th["accent"].Render("✔"), "done"
	case "failed":
		mark, state = th["error"].Render("✗"), "reported failure"
	default:
		mark, state = th["error"].Render("✗"), "error"
	}
	elapsed := s.elapsed
	if s.status == "running" {
		elapsed = time.Since(s.started)
	}
	parts := []string{fmt.Sprintf("subagent %d", s.worker)}
	if b.collapsed {
		task := line(b.label, 44)
		if task == "" {
			task = "task"
		}
		parts = append(parts, task)
	}
	parts = append(parts, state)
	if s.calls > 0 {
		parts = append(parts, plural(s.calls, "call"))
	}
	if elapsed >= time.Second {
		parts = append(parts, fmtElapsed(elapsed))
	}
	if b.collapsed && s.status == "running" && s.last != "" {
		parts = append(parts, line(s.last, 40))
	}
	if s.status != "running" && s.status != "ok" && s.status != "failed" && s.errText != "" {
		parts = append(parts, line(s.errText, 80))
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

	// The body: an accent bar down the left, the task in full, then
	// the heartbeat or the report.
	w := max(m.width-4, 10)
	bar := th["accent"].Render("┃")
	if s.status != "running" {
		bar = th["border"].Render("┃")
	}
	var rows []string
	task := strings.TrimSpace(sanitizeText(b.label))
	if task == "" {
		task = "task"
	}
	for ln := range strings.SplitSeq(th["dim"].Width(w).Render(task), "\n") {
		rows = append(rows, "  "+bar+" "+ln)
	}
	switch {
	case s.status == "running":
		if s.last != "" {
			rows = append(rows, "  "+bar+" "+m.spin.View()+" "+th["dim"].Render(line(s.last, w-2)))
			if s.lastOut != "" {
				rows = append(rows, "  "+bar+"   "+th["dim"].Render(line(s.lastOut, w-4)))
			}
		} else {
			rows = append(rows, "  "+bar+" "+m.spin.View()+" "+th["dim"].Render("starting…"))
		}
		return head + "\n" + strings.Join(rows, "\n")
	case s.errText != "" && s.status != "failed":
		rows = append(rows, "  "+bar+" "+th["error"].Render(line(s.errText, w-2)))
	}
	body := reportBody(sanitizeText(b.text))
	if body == "" {
		body = "(no report)"
	}
	if lines := strings.Split(body, "\n"); len(lines) > reportCap {
		body = strings.Join(lines[:reportCap], "\n") +
			fmt.Sprintf("\n… +%d lines · ctrl+o opens the full transcript", len(lines)-reportCap)
	}
	return head + "\n" + strings.Join(rows, "\n") + "\n" + m.box(body, th["result"], th["border"])
}

// reportBody strips the report contract's scaffolding — a "REPORT"
// heading and the "Status:" line the header already shows — and
// squeezes blank runs, so the capped inline view spends its lines on
// findings.
func reportBody(text string) string {
	var out []string
	for ln := range strings.SplitSeq(strings.TrimSpace(text), "\n") {
		t := strings.TrimSpace(ln)
		lower := strings.ToLower(t)
		if lower == "report" || lower == "# report" || strings.HasPrefix(lower, "status:") {
			continue
		}
		if t == "" && (len(out) == 0 || out[len(out)-1] == "") {
			continue
		}
		out = append(out, ln)
	}
	return strings.TrimSpace(strings.Join(out, "\n"))
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

// hasRunningSpawn reports whether any subagent card is still running.
func (m *model) hasRunningSpawn() bool {
	for i := range m.blocks {
		if b := &m.blocks[i]; b.kind == "spawn" && b.sub != nil && b.sub.status == "running" {
			return true
		}
	}
	return false
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
