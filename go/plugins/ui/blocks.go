package ui

// Transcript block helpers: reasoning-span extraction, human tool
// labels, binary/error text cleanup, turn-end markers, and the View
// panic guard. Rendering policy lives here; model.go only hooks in.

import (
	"slices"

	"github.com/charmbracelet/x/ansi"

	"fmt"
	"regexp"
	"strings"
	"time"
	"unicode"
)

// previewCap: /expand leaves blocks longer than this collapsed unless
// they are focused — one runaway result must not bury the transcript.
const previewCap = 200

// thinkRe matches a <thinking…>…</thinking…> span (any suffix, e.g.
// <thinking_analyses>), closed or running to the end of the text.
var thinkRe = regexp.MustCompile(`(?s)<thinking[^>]*>(.*?)(</thinking[^>]*>|$)`)

// warnRe matches a <system_warning>…</system_warning> span; dropped.
var warnRe = regexp.MustCompile(`(?s)<system_warning>.*?(</system_warning>|$)`)

// splitAssistant turns one assistant reply into blocks: every thinking
// span becomes a collapsed "thinking" block ahead of the prose, system
// warnings vanish, and the remaining prose (if any) is one assistant
// block. Blocks come back without ids; the caller assigns them.
func splitAssistant(text string) []block {
	var out []block
	text = warnRe.ReplaceAllString(text, "")
	for _, m := range thinkRe.FindAllStringSubmatch(text, -1) {
		if body := strings.TrimSpace(m[1]); body != "" {
			out = append(out, block{kind: "thinking", text: body, collapsed: true})
		}
	}
	text = strings.TrimSpace(thinkRe.ReplaceAllString(text, ""))
	if text != "" {
		out = append(out, block{kind: "assistant", text: text})
	}
	return out
}

// fenceRe finds a ```js fence (the loop executes exactly these).
var fenceRe = regexp.MustCompile("(?s)```js[^\\n]*\\n(.*?)```")

// splitAtFence splits prose around the first ```js fence whose body
// matches code (modulo trailing newline): the prose before it, the
// prose after it, and whether a fence matched.
func splitAtFence(text, code string) (before, after string, ok bool) {
	want := strings.TrimRight(code, "\n")
	for _, loc := range fenceRe.FindAllStringSubmatchIndex(text, -1) {
		if strings.TrimRight(text[loc[2]:loc[3]], "\n") == want {
			return strings.TrimSpace(text[:loc[0]]), strings.TrimSpace(text[loc[1]:]), true
		}
	}
	return "", "", false
}

// callRe matches the first tools.<name>(<string literal>?) call.
var callRe = regexp.MustCompile("tools\\.(\\w+)\\(\\s*(?:\"((?:[^\"\\\\]|\\\\.)*)\"|'((?:[^'\\\\]|\\\\.)*)'|`([^`]*)`)?")

// codeLabel derives a human header tag from a code block's tool
// calls: the first recognized call, plus " · " and the second when the
// block makes more than one (an edit followed by the test run reads as
// both, not just "Edited"); "code js" when nothing recognizable leads
// the block.
func codeLabel(code string) string {
	var parts []string
	for _, m := range callRe.FindAllStringSubmatch(code, 3) {
		if l := callLabel(m); l != "" && (len(parts) == 0 || parts[len(parts)-1] != l) {
			parts = append(parts, l)
		}
		if len(parts) == 2 {
			break
		}
	}
	if len(parts) == 0 {
		return "code js"
	}
	return strings.Join(parts, " · ")
}

// callLabel names one callRe match; "" for an unrecognized tool.
func callLabel(m []string) string {
	arg := m[2] + m[3] + m[4]
	if r := []rune(arg); len(r) > 60 {
		arg = string(r[:60]) + "…"
	}
	switch m[1] {
	case "patch":
		return "Edited " + arg
	case "write":
		return "Wrote " + arg
	case "view":
		return "Read " + arg
	case "bash":
		return "Ran: " + strings.SplitN(arg, "\n", 2)[0]
	case "ask":
		return "Asked you"
	case "spawn":
		if arg == "" { // the task came from a variable or a template
			return "Subagent"
		}
		return "Subagent: " + arg
	}
	return ""
}

// binaryText reports whether text is mostly non-printable (a cat of a
// binary), judged on its first 512 bytes.
func binaryText(text string) bool {
	sample := text
	if len(sample) > 512 {
		sample = sample[:512]
	}
	if sample == "" {
		return false
	}
	bad := 0
	n := 0
	for _, r := range sample {
		n++
		if r == '\n' || r == '\t' || r == '\r' {
			continue
		}
		if r == 0 || r == unicode.ReplacementChar || !unicode.IsPrint(r) {
			bad++
		}
	}
	return bad*10 > n*3 // >30%
}

// resultText replaces binary output with a one-line placeholder.
func resultText(text string) string {
	if binaryText(text) {
		return fmt.Sprintf("(binary, %d bytes)", len(text))
	}
	return text
}

// nativeRe is the goja stack suffix on runtime errors ("… at
// github.com/andreylukin/bough/… (native)"): noise, stripped.
var nativeRe = regexp.MustCompile(`(?m)\s+at github\.com/andreylukin/\S+ \(native\)$`)

// goErrRe is goja's wrapper name on a Go-side error ("GoError: bash:
// exit status 1"): runtime plumbing, not something the reader did.
var goErrRe = regexp.MustCompile(`\bGoError: `)

func errorText(text string) string {
	return goErrRe.ReplaceAllString(nativeRe.ReplaceAllString(text, ""), "")
}

// anyFenceRe matches any ``` fence, closed or left open to the end:
// a reply that is nothing but (malformed) fences said nothing.
var anyFenceRe = regexp.MustCompile("(?s)```.*?```|```.*$")

// turnHasReply reports whether the current turn (blocks since the last
// user line) produced anything visible: assistant prose, an error, an
// ask, or a todo. A turn ending without one gets an explicit marker.
func turnHasReply(blocks []block) bool {
	for _, block := range slices.Backward(blocks) {
		switch block.kind {
		case "user":
			return false
		case "assistant":
			if strings.TrimSpace(anyFenceRe.ReplaceAllString(block.text, "")) != "" {
				return true
			}
		case "error", "ask", "todo":
			if strings.TrimSpace(block.text) != "" {
				return true
			}
		case "cancelled":
			return true // "■ cancelled" is the reply; no "without a reply" on top
		}
	}
	return false
}

// doneSummary renders the done entry's files/exit ("✔ wrote a, b ·
// exit 0"); "" when the entry carries neither.
func doneSummary(files []string, exit int, hasExit bool) string {
	var parts []string
	if len(files) > 0 {
		parts = append(parts, "wrote "+strings.Join(files, ", "))
	}
	// A -1 exit is a killed or cancelled command, already reported in
	// its own error row; "✔ exit -1" only read as a contradiction.
	if hasExit && exit >= 0 {
		parts = append(parts, fmt.Sprintf("exit %d", exit))
	}
	if len(parts) == 0 {
		return ""
	}
	mark := "✔"
	if hasExit && exit > 0 {
		mark = "✗"
	}
	return mark + " " + strings.Join(parts, " · ")
}

// safeView runs one frame render, turning a panic into a single error
// line instead of a stack trace in the transcript.
func safeView(render func() string) (out string) {
	defer func() {
		if r := recover(); r != nil {
			line := fmt.Sprint(r)
			if i := strings.IndexByte(line, '\n'); i >= 0 {
				line = line[:i]
			}
			out = "✗ render failed: " + line
		}
	}()
	return render()
}

// sanitizeText makes model/tool text safe to put in a frame: escape
// sequences are stripped (a CSI/OSC in tool output would otherwise
// reach the terminal as-is — hyperlinks, a title, even an OSC 52
// clipboard write), a carriage return keeps only what a terminal
// would show (the text after the last \r on the line), tabs become
// spaces, other control characters are dropped.
func sanitizeText(s string) string {
	if strings.IndexFunc(s, func(r rune) bool { return (r < 0x20 && r != '\n') || r == 0x7f }) < 0 {
		return s
	}
	s = ansi.Strip(s)
	s = strings.ReplaceAll(s, "\r\n", "\n")
	lines := strings.Split(s, "\n")
	for i, ln := range lines {
		if j := strings.LastIndexByte(ln, '\r'); j >= 0 {
			ln = ln[j+1:]
		}
		ln = strings.ReplaceAll(ln, "\t", "    ")
		lines[i] = strings.Map(func(r rune) rune {
			if r < 0x20 || r == 0x7f {
				return -1
			}
			return r
		}, ln)
	}
	return strings.Join(lines, "\n")
}

// --- model hooks (called from model.go) ---

// addAssistant appends the blocks of one assistant reply (see
// splitAssistant), assigning ids.
func (m *model) addAssistant(text string) {
	for _, b := range splitAssistant(text) {
		b.id = m.nextID
		m.nextID++
		m.blocks = append(m.blocks, b)
	}
}

// addDelta grows the turn's live assistant block by one streamed
// fragment, creating it on the first delta. The block is provisional:
// the final "assistant" event replaces it (dropLive + addAssistant),
// which is also where fence splitting and code dedupe happen.
// addThinkDelta grows the live reasoning block. Thinking starts
// collapsed whatever the collapse policy says: it is the model talking
// to itself, one header row until you want it.
func (m *model) addThinkDelta(id int, delta string) {
	if n := len(m.blocks); n > 0 && m.blocks[n-1].live && m.blocks[n-1].kind == "thinking" {
		m.blocks[n-1].text += delta
		return
	}
	m.blocks = append(m.blocks, block{id: id, kind: "thinking", text: delta, live: true, collapsed: true})
}

// finishThinking settles the live reasoning block on the final text.
func (m *model) finishThinking(id int, text string) {
	for i := len(m.blocks) - 1; i >= 0; i-- {
		if b := &m.blocks[i]; b.kind == "thinking" && b.live {
			b.text, b.live = text, false
			return
		}
	}
	m.blocks = append(m.blocks, block{id: id, kind: "thinking", text: text, collapsed: true})
}

func (m *model) addDelta(id int, delta string) {
	if n := len(m.blocks); n > 0 && m.blocks[n-1].live && m.blocks[n-1].kind == "assistant" {
		m.blocks[n-1].text += delta
		return
	}
	m.blocks = append(m.blocks, block{id: id, kind: "assistant", text: delta, live: true})
}

// dropLive removes the provisional streaming block, if any.
func (m *model) dropLive() {
	if n := len(m.blocks); n > 0 && m.blocks[n-1].live && m.blocks[n-1].kind == "assistant" {
		m.blocks = m.blocks[:n-1]
	}
}

// liveView is what a streaming reply shows: the prose before the first
// code fence, and whether a fence has started. A fence opener still
// arriving ("`", "“") is held back too, so the tail never flickers
// between backticks and the note. Pure.
func liveView(text string) (prose string, coding bool) {
	if before, _, ok := strings.Cut(text, "```"); ok {
		return strings.TrimRight(before, "\n `"), true
	}
	return strings.TrimRight(text, "`"), false
}

// splitProse removes the executed fence from an assistant block: the
// prose before it stays, the prose after it is held back (m.trailing)
// until the turn ends (or an ask needs it), so the transcript reads in
// emission order; a further reply in the turn supersedes it (addEvent). Falls back to a plain strip for a non-js fence.
func (m *model) splitProse(b *block, want string) (string, bool) {
	if before, after, ok := splitAtFence(b.text, want); ok {
		if after != "" {
			m.trailing = strings.TrimSpace(m.trailing + "\n\n" + after)
		}
		return before, true
	}
	return stripFence(b.text, want)
}

// flushTrailing appends the held-back prose (if any) as an assistant
// block.
func (m *model) flushTrailing() {
	if m.trailing == "" {
		return
	}
	text := m.trailing
	m.trailing = ""
	m.addAssistant(text)
}

// finishTurn appends the turn-end marker(s) for a done event: an
// explicit "turn ended without a reply" error when nothing visible was
// produced, then the done block carrying the entry's files/exit. A
// queued user line, if any, starts now.
func (m *model) finishTurn(id int, ev Event) {
	if !turnHasReply(m.blocks) {
		m.blocks = append(m.blocks, block{id: id, kind: "error", text: "turn ended without a reply"})
		id = m.nextID
		m.nextID++
	}
	b := block{id: id, kind: "done", files: strList(ev.Data["files"])}
	switch v := ev.Data["exit"].(type) {
	case int:
		b.exit = &v
	case int64:
		e := int(v)
		b.exit = &e
	case float64:
		e := int(v)
		b.exit = &e
	}
	m.blocks = append(m.blocks, b)
	// A queued line starts now. (A steer never outlives its turn: the
	// loop lands every accepted one — "steer" event — before the done.)
	for i := range m.blocks {
		if m.blocks[i].queued {
			m.blocks[i].queued = false
			m.running = true
			m.turnStart = time.Now()
			break
		}
	}
}

// renderUser wraps the prompt to the pane width; a queued line says
// so, and a steer says whether the loop has picked it up yet.
func (m *model) renderUser(b *block, th theme) string {
	text := "❯ " + b.text
	switch {
	case b.queued:
		text += " (queued)"
	case b.pending:
		text += " (steer · pending)"
	case b.steer:
		text += " (steer)"
	}
	st := th["user"]
	if m.width >= 10 {
		st = st.Width(m.width)
	}
	return st.Render(text)
}

// renderDone is the dim files/exit summary (when the entry carries
// one) over the turn divider.
func (m *model) renderDone(b *block, th theme) string {
	w := max(min(m.width-2, 40), 1)
	out := th["dim"].Render(strings.Repeat("─", w))
	exit, hasExit := 0, false
	if b.exit != nil {
		exit, hasExit = *b.exit, true
	}
	if s := doneSummary(b.files, exit, hasExit); s != "" {
		out = th["dim"].Render(s) + "\n" + out
	}
	return out
}

// mayExpand: expanding a block over previewCap lines needs focus.
func (m *model) mayExpand(b *block, collapsed bool) bool {
	return collapsed || strings.Count(b.text, "\n")+1 <= previewCap || b.id == m.focusID
}

// collapseNote is the feedback line for collapse/expand-all.
func collapseNote(collapsed bool, n int) string {
	unit := "blocks"
	if n == 1 {
		unit = "block"
	}
	if n == 0 {
		if collapsed {
			return "nothing to collapse: every step is already folded"
		}
		return "nothing to expand: every step is already open"
	}
	if collapsed {
		return fmt.Sprintf("collapsed %d %s", n, unit)
	}
	return fmt.Sprintf("expanded %d %s (blocks over %d lines stay collapsed unless focused)", n, unit, previewCap)
}

// noteSystem appends a system row and pins the transcript to it.
func (m *model) noteSystem(text string) {
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: text})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
}

// scrollCue is the status-bar hint while the transcript is scrolled
// up: how far, and whether output landed below since.
func (m *model) scrollCue() string {
	if m.inspecting || m.vp.AtBottom() {
		return ""
	}
	below := m.vp.TotalLineCount() - m.vp.YOffset() - m.vp.Height()
	cue := fmt.Sprintf("scrolled ↑ %d lines", below)
	if m.newBelow {
		cue = "↓ new output · " + cue
	}
	return cue
}

// colorDiff colors the +/- lines of an edit result ("patched …" or
// "wrote …" followed by a diff, see tools.lineDiff): added lines in
// the accent, removed in the error color, so an edit reads like a
// diff rather than a dump. Other results pass through.
func colorDiff(text string, th theme) string {
	if !strings.HasPrefix(text, "patched ") && !strings.HasPrefix(text, "wrote ") {
		return text
	}
	lines := strings.Split(text, "\n")
	for i, l := range lines[1:] {
		switch {
		case strings.HasPrefix(l, "+"):
			lines[i+1] = th["accent"].Render(l)
		case strings.HasPrefix(l, "-"):
			lines[i+1] = th["error"].Render(l)
		}
	}
	return strings.Join(lines, "\n")
}
