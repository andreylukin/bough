package ui

import (
	"image/color"
	"slices"
	"time"

	xansi "github.com/charmbracelet/x/ansi"

	"encoding/json"
	"fmt"
	"regexp"
	"strings"
	"sync/atomic"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2"
	"charm.land/glamour/v2/ansi"
	"charm.land/glamour/v2/styles"
	"charm.land/lipgloss/v2"
)

// eventMsg carries a loop event into the tea event loop.
type eventMsg Event

// collapseAt: code and result blocks whose body is longer than this
// many lines start collapsed (header line only).
const collapseAt = 3

// block is one semantic transcript unit, stored structurally and
// styled at render time from the current theme. id is stable identity
// (blocks are append-only) so collapse/focus state survives appends.
type block struct {
	id        int
	kind      string // user, assistant, code, result, error, done, ask, ...
	text      string
	label     string // header tag override ("! <cmd>" bang blocks); "" = by kind
	collapsed bool
	queued    bool     // user block submitted mid-turn, not yet started
	files     []string // done blocks: files the turn wrote (from the entry's data)
	exit      *int     // done blocks: exit status, nil when absent

	// ask blocks only (see ask.go): the pending question's options and
	// id, and how it resolved.
	askID    string
	options  []string
	answer   string
	answered bool
	expired  bool // turn ended (or replay found no answer entry) unanswered

	// spawn blocks only (see spawn.go): the subagent card's live state;
	// label is the task, text the child's report.
	sub  *subState
	live bool // assistant text still streaming (see addDelta)
}

// collapsible blocks get a disclosure header and can be toggled.
func (b *block) collapsible() bool {
	return b.kind == "code" || b.kind == "result" || b.kind == "thinking" || b.kind == "spawn"
}

// lineRange maps a rendered line span [start, end) to a block index.
type lineRange struct {
	start, end, idx int
}

// model is the one transcript-plus-composer model used by tui and web.
type model struct {
	vp      viewport.Model
	overlay viewport.Model
	input   textarea.Model
	spin    spinner.Model
	events  <-chan Event
	send    func(string)
	cfg     *atomic.Pointer[uiCfg]

	blocks     []block
	nextID     int
	focusID    int // block-cursor identity; -1 when nothing focused
	ranges     []lineRange
	width      int
	height     int
	running    bool           // a turn is in flight (input sent, no done/error yet)
	turnStart  time.Time      // when the in-flight turn started (status bar elapsed)
	inspecting bool           // history overlay open
	diving     int            // spawn card id whose child transcript the overlay shows (0 = history)
	ovRanges   []lineRange    // overlay line span -> entry index
	ovExpanded map[int64]bool // entry seq -> inline JSON shown
	ovEntries  []int64        // entry index -> seq, for ovRanges lookups
	picking    bool           // session picker shown instead of the chat view
	pick       int            // picker cursor index into cfg.sessions
	mp         modelPicker    // "/model" picker (see modelpick.go)
	todoText   string         // latest todo list text (the todo plugin's event)
	todoPinned bool           // todo list pinned above the composer (todo_toggle)
	sessRows   sessList       // mid-session picker list (see session.go); nil = launch picker
	welcome    bool           // fresh-session orientation text (see welcomeView)
	pendingAsk string         // ask id the composer routes answers to; "" = none
	pal        palette        // "/" command palette (see palette.go)
	at         palette        // "@" file picker (see atfiles.go)
	atFiles    []string       // the picker's file list, read when it opens
	flash      string
	trailing   string        // assistant prose after an executed fence, emitted after its result
	newBelow   bool          // blocks arrived while scrolled up (status cue)
	sel        selection     // mouse drag selection (see select.go)
	lines      []string      // rendered content lines, for the selection
	stop       stopState     // quit-key arming (see stop.go)
	comp       composerState // prompt recall (see composer.go)
	md         *glamour.TermRenderer
	mdCache    map[string]string // assistant markdown render cache (cleared on resize)
	bgLight    bool              // terminal background is light (tea.BackgroundColorMsg)
}

func newModel(width, height int, send func(string), events <-chan Event, cfg *atomic.Pointer[uiCfg]) model {
	ti := newComposer()

	vp := viewport.New()
	vp.KeyMap = viewport.KeyMap{} // all keys resolve through the keymap service
	ov := viewport.New()
	ov.KeyMap = viewport.KeyMap{}

	sp := spinner.New()
	sp.Spinner = spinner.MiniDot

	m := model{vp: vp, overlay: ov, input: ti, spin: sp, send: send, events: events, cfg: cfg,
		focusID: -1, ovExpanded: map[int64]bool{}, mdCache: map[string]string{}, comp: composerState{recall: -1}}
	m.resize(width, height)
	if cfg.Load().picker {
		m.picking = true // replay happens after the pick (leavePicker)
	} else {
		m.replay()
	}
	return m
}

func (m *model) resize(w, h int) {
	m.width = w
	m.height = h
	if h > 2 {
		m.vp.SetHeight(h - 2)
		m.overlay.SetHeight(h - 2)
	}
	m.vp.SetWidth(w)
	m.overlay.SetWidth(w)
	// A short pane caps the composer below composerMaxLines so status
	// bar + composer + one transcript row always fit.
	m.input.MaxHeight = max(1, min(composerMaxLines, h-2))
	m.input.SetWidth(max(w, 1))
	m.md = nil // re-wrap markdown at the new width
	m.mdCache = map[string]string{}
	m.refresh()
	if m.inspecting {
		m.refreshOverlay()
	}
}

// refresh restyles every block from the current theme, records each
// block's rendered line span for mouse hit-testing, and keeps the
// scroll position (still pinned when it was at the bottom). Parts are
// blank-squeezed so at most one blank line separates blocks.
func (m *model) refresh() {
	m.layoutComposer()
	cfg := m.cfg.Load()
	if m.welcome && len(m.blocks) == 0 {
		// Fresh session: orientation text instead of an empty pane. Not
		// a block (no hit-test ranges, never in history); it vanishes as
		// soon as anything lands in the transcript, and /clear drops it.
		m.ranges = m.ranges[:0]
		m.vp.SetContent(m.welcomeView(cfg))
		return
	}
	atBottom := m.vp.AtBottom()
	parts := make([]string, 0, len(m.blocks))
	m.ranges = m.ranges[:0]
	start := 0
	for i := range m.blocks {
		part := squeezeBlanks(m.render(&m.blocks[i], cfg))
		if i == 0 {
			part = strings.TrimLeft(part, "\n")
		}
		n := strings.Count(part, "\n") + 1
		m.ranges = append(m.ranges, lineRange{start: start, end: start + n, idx: i})
		start += n
		parts = append(parts, part)
	}
	m.lines = strings.Split(strings.Join(parts, "\n"), "\n")
	m.vp.SetContent(strings.Join(m.highlight(m.lines, cfg), "\n"))
	if atBottom {
		m.vp.GotoBottom()
	}
}

// mdStyles is glamour's dark or light style with inline code recolored
// from the theme: the stock red-on-grey fought every palette. Inline
// code takes the accent on the status bar's ground.
func mdStyles(style string, th theme) ansi.StyleConfig {
	cfg := styles.DarkStyleConfig
	if style == "light" {
		cfg = styles.LightStyleConfig
	}
	if fg := hexOf(th["accent"].GetForeground()); fg != "" {
		cfg.Code.Color = &fg
	}
	if bg := hexOf(th["status"].GetBackground()); bg != "" {
		cfg.Code.BackgroundColor = &bg
	}
	return cfg
}

// hexOf renders a color as "#rrggbb"; "" for none.
func hexOf(c color.Color) string {
	if c == nil {
		return ""
	}
	r, g, b, a := c.RGBA()
	if a == 0 {
		return ""
	}
	return fmt.Sprintf("#%02x%02x%02x", r>>8, g>>8, b>>8)
}

// markdown renders assistant text via glamour, falling back to the
// raw text on any error. The style follows the detected terminal
// background (dark until told otherwise); a theme service "markdown"
// entry ("dark"/"light") overrides detection.
func (m *model) markdown(text string) string {
	if cached, ok := m.mdCache[text]; ok {
		return cached
	}
	if m.md == nil {
		w := max(m.width-2, 20)
		style := "dark"
		if m.bgLight {
			style = "light"
		}
		if s := m.cfg.Load().mdStyle; s != "" {
			style = s
		}
		r, err := glamour.NewTermRenderer(
			glamour.WithStyles(mdStyles(style, m.cfg.Load().theme)),
			glamour.WithWordWrap(w),
			glamour.WithEmoji(),
			// Hard newlines survive: "[tool output]\nhi" is two lines,
			// not one reflowed paragraph.
			glamour.WithPreservedNewLines(),
		)
		if err != nil {
			return text
		}
		m.md = r
	}
	out, err := m.md.Render(text)
	if err != nil {
		return text
	}
	out = strings.Trim(out, "\n")
	m.mdCache[text] = out
	return out
}

// squeezeBlanks trims trailing blank lines from a rendered part and
// collapses interior runs of blank lines to a single one, keeping the
// transcript to at most one blank line between blocks.
func squeezeBlanks(s string) string {
	lines := strings.Split(s, "\n")
	out := lines[:0]
	blanks := 0
	for _, l := range lines {
		if strings.TrimSpace(l) == "" {
			if blanks++; blanks > 1 {
				continue
			}
		} else {
			blanks = 0
		}
		out = append(out, l)
	}
	for len(out) > 0 && strings.TrimSpace(out[len(out)-1]) == "" {
		out = out[:len(out)-1]
	}
	return strings.Join(out, "\n")
}

// header renders the one-line disclosure header for a collapsible
// block: glyph, kind tag, body line count, first-line preview. The
// focused block's header takes the "focus" theme style.
func (m *model) header(b *block, th theme) string {
	glyph := "▸"
	if !b.collapsed {
		glyph = "▾"
	}
	tag := "result"
	switch b.kind {
	case "code":
		tag = codeLabel(b.text)
	case "thinking":
		tag = "thinking"
	}
	if b.label != "" {
		tag = b.label
	}
	n := strings.Count(b.text, "\n") + 1
	unit := "lines"
	if n == 1 {
		unit = "line"
	}
	// A recognized call ("Ran: …", "Edited …") already says what the
	// code does: the raw `console.log(tools.bash(…` preview underneath
	// it only leaked internals. Unlabeled code and results keep their
	// first line as the preview.
	head := fmt.Sprintf("%s %s (%d %s)", glyph, tag, n, unit)
	if b.kind != "code" || tag == "code js" {
		head += ": " + strings.SplitN(b.text, "\n", 2)[0]
	}
	if r := []rune(head); len(r) > m.width-1 && m.width > 2 {
		head = string(r[:m.width-2]) + "…"
	}
	st := th["dim"]
	if b.id == m.focusID {
		st = th["focus"]
	}
	return st.Render(head)
}

// welcomeView is the fresh-session orientation text (tui/web only):
// shown while the transcript is empty on a session with no history,
// suppressed on resume, gone once anything renders, removed by /clear.
func (m *model) welcomeView(cfg *uiCfg) string {
	th := cfg.theme
	return th["accent"].Render("● ") + th["dim"].Render("bough — a coding agent") + "\n" +
		th["dim"].Render("  type / for commands (/help lists them) · /keys or ? for the keys") + "\n" +
		th["dim"].Render("  enter sends · ctrl+j or a trailing \\ starts a new line · esc stops a turn") + "\n" +
		th["dim"].Render("  tab picks a ▸ step, enter expands it · !cmd runs a shell command") + "\n" +
		th["dim"].Render("  ask me to do something — I act by running code")
}

// authErrRe spots credential-shaped failures in error text; the match
// appends the credential hint below the error block.
var authErrRe = regexp.MustCompile(`(?i)\b40[13]\b|unauthorized|credentials|api[ _-]?key|x-api-key`)

const authHint = "hint: check your provider credentials (ANTHROPIC_API_KEY / OPENROUTER_API_KEY), or swap the llm row — /model"

// render turns one semantic block into styled lines.
func (m *model) render(b *block, cfg *uiCfg) string {
	// Block text is stored raw (a copy must be the true output); the
	// frame gets a sanitized view of it.
	if s := sanitizeText(b.text); s != b.text {
		c := *b
		c.text = s
		b = &c
	}
	th := cfg.theme
	switch b.kind {
	case "user":
		return "\n" + m.renderUser(b, th)
	case "assistant":
		head := th["accent"].Render("●") + " " + th["dim"].Render("bough")
		if b.live {
			// Streaming: the prose so far, plain, with a cursor; markdown
			// waits for the finished reply. A code fence being written
			// is NOT shown: it would type out as text and then jump into
			// a collapsed code block when the reply lands. A dim note
			// stands in for it until then.
			prose, coding := liveView(b.text)
			out := head
			if prose != "" || !coding {
				out += "\n" + th["assistant"].Width(m.width).Render(prose+"▌")
			}
			if coding {
				out += "\n" + th["dim"].Render("▸ writing code…")
			}
			return out
		}
		return head + "\n" + m.markdown(b.text)
	case "code", "thinking":
		if b.collapsed {
			return m.header(b, th)
		}
		return m.header(b, th) + "\n" + m.box(b.text, th["code"], th["border"])
	case "result":
		if b.collapsed {
			return m.header(b, th)
		}
		return m.header(b, th) + "\n" + m.box(colorDiff(b.text, th), th["result"], th["border"])
	case "spawn":
		return m.renderSpawn(b, th)
	case "command":
		// The dispatched "/" line: a dim echo of what was typed, so
		// the system block below reads as its answer.
		return "\n" + th["dim"].Render("❯ "+b.text)
	case "system":
		// Plain dimmed command output: not collapsible, no ● header;
		// wrapped to width so /help rows never clip off the right edge.
		return th["system"].Width(max(m.width, 10)).Render(b.text)
	case "error":
		// Wrap to width — the viewport clips long lines, and the tail
		// of an error is usually the actionable part.
		w := max(m.width, 10)
		out := th["error"].Width(w).Render("✗ " + b.text)
		if authErrRe.MatchString(b.text) {
			out += "\n" + th["dim"].Width(w).Render(authHint)
		}
		return out
	case "todo":
		// Dedicated todo render: dim tag + checkbox lines, done items
		// dimmed. See addEvent for the one-render-per-mutation rule.
		lines := strings.Split(b.text, "\n")
		for i, l := range lines {
			if strings.HasPrefix(l, "[x]") {
				lines[i] = th["dim"].Render(l)
			}
		}
		return th["dim"].Render("todo") + "\n" + strings.Join(lines, "\n")
	case "ask":
		return m.renderAsk(b, th)
	case "done":
		return m.renderDone(b, th)
	case "cancelled":
		return th["accent"].Bold(true).Render("■ cancelled") + th["dim"].Render(" — stopped by you")
	default:
		return th["dim"].Render(b.kind+" ") + b.text
	}
}

// box renders text in a rounded border.
func (m *model) box(text string, content, border lipgloss.Style) string {
	w := max(m.width-4, 10)
	return content.
		Border(lipgloss.RoundedBorder()).
		BorderForeground(border.GetForeground()).
		Padding(0, 1).
		Width(w).
		Render(strings.TrimRight(text, "\n"))
}

// waitEvent blocks for the next loop event.
func (m model) waitEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.events
		if !ok {
			return nil
		}
		return eventMsg(ev)
	}
}

func (m model) Init() tea.Cmd {
	return tea.Batch(m.waitEvent(), tea.RequestBackgroundColor)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.resize(msg.Width, msg.Height)
		return m, nil

	case spinner.TickMsg:
		if !m.running {
			return m, nil
		}
		var cmd tea.Cmd
		m.spin, cmd = m.spin.Update(msg)
		// The transcript pane is content set at refresh time, not
		// drawn per frame like the status bar: a running subagent
		// card's spinner and elapsed only move if the pane is rebuilt
		// on the tick.
		if m.hasRunningSpawn() {
			m.refresh()
		}
		return m, cmd

	case eventMsg:
		m.addEvent(Event(msg))
		return m, m.waitEvent()

	case bangDoneMsg:
		m.finishBang(msg)
		return m, nil

	case tea.BackgroundColorMsg:
		// Re-render markdown for the actual terminal background so
		// dark-style grays never land on a light terminal.
		if light := !msg.IsDark(); light != m.bgLight {
			m.bgLight = light
			m.md = nil
			m.mdCache = map[string]string{}
			m.refresh()
		}
		return m, nil

	case tea.KeyPressMsg:
		return m.handleKey(msg)

	case tea.MouseClickMsg:
		return m, m.handleClick(msg.Mouse())

	case tea.MouseMotionMsg:
		m.dragSelect(msg.Mouse())
		return m, nil // never the composer's business

	case tea.MouseReleaseMsg:
		return m, m.releaseSelect(msg.Mouse())

	case tea.PasteMsg:
		m.stop.armedAt = time.Time{} // a paste is typing: it disarms quit like any key
	}

	var cmds []tea.Cmd
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette() // e.g. paste can change the draft
	m.layoutComposer()
	cmds = append(cmds, cmd)
	if m.inspecting {
		m.overlay, cmd = m.overlay.Update(msg)
	} else {
		m.vp, cmd = m.vp.Update(msg)
	}
	cmds = append(cmds, cmd)
	return m, tea.Batch(cmds...)
}

// addEvent appends the semantic block for a loop event. Whether code
// and result blocks start collapsed follows cfg.collapse: "all"
// (default) collapses every one, "large" only those over collapseAt
// lines, "none" leaves them expanded.
func (m *model) addEvent(ev Event) {
	id := m.nextID
	m.nextID++
	switch ev.Kind {
	case "assistant", "assistant-delta":
		// The model speaks again after seeing its results: the prose
		// it wrote under the fence BEFORE seeing them ("Done, here's
		// the file:") is superseded, not a second answer. Drop it.
		m.trailing = ""
	case "done", "ask":
		// The turn ends (or pauses on the user) on that prose: it is
		// the model's last word, so it shows. Mid-turn events (code,
		// results, subagent activity, todo updates) keep it held.
		m.flushTrailing()
	}
	switch ev.Kind {
	case "done":
		m.running = false
		m.expireAsks() // a turn never ends with a live ask
		if m.flash == "cancelling…" {
			m.flash = "" // the cancel landed: the transcript says so, the bar goes back to its chips
		}
		m.finishTurn(id, ev)
	case "error":
		// Not the end of the turn: a failed code block is fed back to
		// the model, which usually carries on (the loop always closes a
		// turn with "done", which is what stops the spinner and expires
		// asks). Ending the turn here froze the spinner mid-run and
		// hid the recovery that followed.
		m.blocks = append(m.blocks, block{id: id, kind: "error", text: errorText(ev.Text)})
		m.flushTrailing()
	case "ask":
		m.blocks = append(m.blocks, block{id: id, kind: "ask", text: ev.Text,
			askID: ev.ID, options: ev.Options})
		m.pendingAsk = ev.ID
		m.input.Placeholder = askPlaceholder
	case "code", "result":
		if ev.Kind == "code" {
			m.dedupeCode(ev.Text)
		}
		// Trailing newlines don't render (box trims them); don't let
		// them skew the header's line count or the collapse default.
		text := strings.TrimRight(ev.Text, "\n")
		if ev.Kind == "result" {
			text = resultText(text)
		}
		var collapsed bool
		switch m.cfg.Load().collapse {
		case "none":
		case "large":
			collapsed = strings.Count(text, "\n")+1 > collapseAt
		default: // "all"
			collapsed = true
		}
		m.blocks = append(m.blocks, block{id: id, kind: ev.Kind, text: text, collapsed: collapsed})
	case "sub:start", "sub:assistant", "sub:code", "sub:result", "sub:error", "sub:done":
		// A subagent's activity folds into ONE card per worker (spawn.go):
		// the parent's transcript is the story, the child's is detail
		// behind the card.
		m.addSubEvent(ev)
	case "assistant-delta":
		m.addDelta(id, ev.Text)
	case "assistant":
		m.dropLive()
		m.addAssistant(ev.Text)
	case "todo":
		m.todoText = ev.Text
		// One render per mutation: the system block a /todo mutation
		// just printed (same text) becomes the todo block, and
		// back-to-back todo events (several mutations in one script)
		// update one block instead of stacking copies.
		if n := len(m.blocks); n > 0 {
			if last := &m.blocks[n-1]; last.kind == "todo" ||
				(last.kind == "system" && last.text == ev.Text) {
				last.kind, last.text = "todo", ev.Text
				m.refresh()
				m.vp.GotoBottom()
				return
			}
		}
		m.blocks = append(m.blocks, block{id: id, kind: "todo", text: ev.Text})
	default: // assistant, anything future
		m.blocks = append(m.blocks, block{id: id, kind: ev.Kind, text: ev.Text})
	}
	m.refresh() // pins to the bottom only when it already was there
	if !m.vp.AtBottom() {
		m.newBelow = true
	}
}

// dedupeCode strips, from this turn's assistant blocks, the fenced
// code block whose body matches an executed code event (exact modulo
// trailing newline): the collapsible code block is the single
// rendering of executed code. An assistant block left with only
// whitespace is dropped entirely — no orphan "● bough" header.
func (m *model) dedupeCode(code string) {
	want := strings.TrimRight(code, "\n")
	// A multi-fence reply: the fence sits in the held prose after an
	// earlier fence. The prose before it is now in order (that
	// fence's result has landed) and shows; the rest stays held.
	if before, after, ok := splitAtFence(m.trailing, want); ok {
		m.trailing = ""
		if before != "" {
			m.addAssistant(before)
		}
		m.trailing = after
		return
	}
	for i := len(m.blocks) - 1; i >= 0; i-- {
		b := &m.blocks[i]
		switch b.kind {
		case "user", "done", "error":
			return // turn boundary
		case "assistant":
			txt, ok := m.splitProse(b, want)
			if !ok {
				continue
			}
			if strings.TrimSpace(txt) == "" {
				m.blocks = append(m.blocks[:i], m.blocks[i+1:]...)
			} else {
				b.text = txt
			}
			return
		}
	}
}

// stripFence removes the first ```-fenced block whose body equals want
// (modulo trailing newline) from text, reporting whether one matched.
func stripFence(text, want string) (string, bool) {
	lines := strings.Split(text, "\n")
	for i := 0; i < len(lines); i++ {
		if !strings.HasPrefix(strings.TrimSpace(lines[i]), "```") {
			continue
		}
		j := i + 1
		for j < len(lines) && strings.TrimSpace(lines[j]) != "```" {
			j++
		}
		if j < len(lines) && strings.TrimRight(strings.Join(lines[i+1:j], "\n"), "\n") == want {
			out := slices.Concat(lines[:i], lines[j+1:])
			return strings.Join(out, "\n"), true
		}
		i = j // skip past this fence (or to EOF)
	}
	return "", false
}

// handleClick maps a left click to the block (or history entry) under
// it and toggles its collapsed state. Wheel scrolling stays with the
// viewports (they handle MouseWheelMsg themselves).
func (m *model) handleClick(mouse tea.Mouse) tea.Cmd {
	if mouse.Button != tea.MouseLeft || m.picking || m.mp.open {
		return nil
	}
	if m.inspecting {
		m.clickOverlay(mouse)
		return nil
	}
	if handled, cmd := m.clickPalette(mouse); handled {
		return cmd
	}
	if mouse.Y == m.vp.Height() && m.cfg.Load().hist != nil {
		m.openPicker() // status bar names the session: click to switch
		return nil
	}
	if mouse.Y >= m.vp.Height() {
		return nil // composer
	}
	m.pressSelect(mouse) // acts on release unless the mouse moves (drag = select)
	return nil
}

// clickTranscript is a plain click (press and release without a drag)
// on a transcript row: answer an ask option, else toggle the block.
func (m *model) clickTranscript(y int) tea.Cmd {
	row := y + m.vp.YOffset()
	for _, r := range m.ranges {
		if row >= r.start && row < r.end {
			b := &m.blocks[r.idx]
			if b.kind == "ask" && b.askID == m.pendingAsk && !b.answered && !b.expired {
				// Option rows sit directly under the question line.
				if off := row - r.start; off >= 1 && off <= len(b.options) {
					m.answerAsk(b, b.options[off-1])
				}
				return nil
			}
			if b.collapsible() {
				m.toggleBlock(r.idx)
			}
			return nil
		}
	}
	return nil
}

// clickOverlay toggles the inline pretty-JSON view of the history
// entry under the click.
func (m *model) clickOverlay(mouse tea.Mouse) {
	if mouse.Y >= m.overlay.Height() || m.diving != 0 {
		return
	}
	row := mouse.Y + m.overlay.YOffset()
	for _, r := range m.ovRanges {
		if row >= r.start && row < r.end {
			seq := m.ovEntries[r.idx]
			m.ovExpanded[seq] = !m.ovExpanded[seq]
			m.refreshOverlay()
			return
		}
	}
}

// focusables returns the indices of collapsible blocks, in order.
func (m *model) focusables() []int {
	var out []int
	for i := range m.blocks {
		if m.blocks[i].collapsible() {
			out = append(out, i)
		}
	}
	return out
}

// moveFocus steps the block cursor over the collapsible blocks
// (wrapping), scrolling the focused header into view. With nothing
// focused it starts at the newest block; delta +1 (tab) then walks
// older, -1 (shift+tab) newer.
func (m *model) moveFocus(delta int) {
	f := m.focusables()
	if len(f) == 0 {
		return
	}
	cur := -1
	for i, idx := range f {
		if m.blocks[idx].id == m.focusID {
			cur = i
			break
		}
	}
	var next int
	if cur < 0 {
		// Nothing focused: start from the NEWEST block (the one you were
		// just looking at), whichever way you step. Starting at the top
		// of the transcript yanked the view to the oldest block.
		next = len(f) - 1
	} else {
		// tab (delta +1) walks OLDER, up the transcript from where you
		// are; shift+tab walks back toward the newest.
		next = (cur - delta + len(f)) % len(f)
	}
	m.focusID = m.blocks[f[next]].id
	m.refresh()
	for _, r := range m.ranges {
		if r.idx == f[next] {
			m.vp.EnsureVisible(r.start, 0, 0)
			break
		}
	}
}

// toggleFocused flips the focused block's collapsed state; false when
// nothing is focused.
func (m *model) toggleFocused() bool {
	for i := range m.blocks {
		if m.blocks[i].id == m.focusID && m.blocks[i].collapsible() {
			m.toggleBlock(i)
			return true
		}
	}
	return false
}

// toggleBlock flips block i, focuses it, and keeps its header on
// screen: with the transcript pinned to the bottom, expanding a long
// block used to scroll the header you just clicked out of view.
func (m *model) toggleBlock(i int) {
	m.blocks[i].collapsed = !m.blocks[i].collapsed
	m.focusID = m.blocks[i].id
	m.refresh()
	for _, r := range m.ranges {
		if r.idx == i {
			m.vp.EnsureVisible(r.start, 0, 0)
			break
		}
	}
}

// setAllCollapsed collapses or expands every collapsible block,
// returning how many changed. Expanding skips blocks over previewCap
// lines unless focused (see blocks.go).
func (m *model) setAllCollapsed(collapsed bool) int {
	n := 0
	for i := range m.blocks {
		if b := &m.blocks[i]; b.collapsible() && b.collapsed != collapsed && m.mayExpand(b, collapsed) {
			b.collapsed = collapsed
			n++
		}
	}
	m.refresh()
	return n
}

// handleKey resolves every binding through the keymap service; only
// enter (submit — not a remappable action) is fixed. Enter first acts
// as collapse_toggle when a block is focused, and submits otherwise.
func (m model) handleKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	if m.picking {
		return m.handlePickerKey(msg)
	}
	if m.mp.open {
		return m.handleModelPickerKey(msg)
	}
	cfg := m.cfg.Load()
	m.flash = ""
	key := msg.String()
	if cfg.action[key] != "quit" {
		m.stop.armedAt = time.Time{} // any other key disarms, whichever handler takes it
	}

	// The palette owns Up/Down/Tab/Enter/Esc while it is open, and
	// nothing else: what it passes on falls through to the keymap
	// waterfall and the composer (which re-filters).
	if m.pal.open && !m.inspecting {
		if handled, cmd := m.paletteKey(key); handled {
			return m, cmd
		}
	}
	if m.at.open && !m.inspecting {
		if handled, cmd := m.atKey(key); handled {
			return m, cmd
		}
	}

	// esc closes a subagent dive (the history inspector keeps its own key).
	if key == "esc" && m.inspecting && m.diving != 0 {
		m.inspecting, m.diving = false, 0
		m.syncPalette()
		return m, nil
	}

	// A pending ask owns esc: decline it (the literal "(declined)" is
	// the tool's return value, so the model knows it was waved off).
	if key == "esc" && m.pendingAsk != "" && !m.inspecting {
		m.answerPending("(declined)")
		return m, nil
	}

	// "?" on an empty composer is the keymap (/keys), not a typed "?".
	if key == "?" && !m.inspecting && strings.TrimSpace(m.input.Value()) == "" {
		m.showKeys()
		return m, nil
	}
	if handled, cmd := m.stopKey(key, cfg); handled {
		return m, cmd
	}
	if handled, cmd := m.composerKey(key, msg); handled {
		return m, cmd
	}

	switch cfg.action[key] {
	case "quit":
		return m, tea.Quit
	case "scroll_up":
		m.pane().ScrollUp(1)
		return m, nil
	case "scroll_down":
		m.pane().ScrollDown(1)
		return m, nil
	case "page_up":
		m.pane().PageUp()
		return m, nil
	case "page_down":
		m.pane().PageDown()
		return m, nil
	case "clear_input":
		m.input.Reset()
		m.syncPalette()
		m.layoutComposer()
		return m, nil
	case "block_next":
		if !m.inspecting {
			m.moveFocus(1)
		}
		return m, nil
	case "block_prev":
		if !m.inspecting {
			m.moveFocus(-1)
		}
		return m, nil
	case "collapse_all":
		m.flash = collapseNote(true, m.setAllCollapsed(true))
		return m, nil
	case "expand_all":
		m.flash = collapseNote(false, m.setAllCollapsed(false))
		return m, nil
	case "collapse_toggle":
		if key == "enter" && strings.TrimSpace(m.input.Value()) != "" {
			break // composing: enter submits below
		}
		if !m.inspecting && m.toggleFocused() {
			return m, nil
		}
		// nothing focused: enter falls through to submit below; any
		// other bound key toggles the newest collapsible block.
		if key != "enter" {
			if f := m.focusables(); !m.inspecting && len(f) > 0 {
				m.toggleBlock(f[len(f)-1])
			}
			return m, nil
		}
	case "todo_toggle":
		if m.todoText == "" {
			m.flash = "no todo list yet (/todo add <text>)"
			return m, nil
		}
		m.todoPinned = !m.todoPinned
		return m, nil
	case "history_inspect":
		if m.inspecting {
			m.inspecting, m.diving = false, 0
			m.syncPalette()
			return m, nil
		}
		// On a focused subagent card: dive into the child's transcript.
		if i := m.focusedSpawn(); i >= 0 {
			m.inspecting, m.diving = true, m.blocks[i].id
			m.refreshOverlay()
			m.overlay.GotoTop()
			m.syncPalette()
			return m, nil
		}
		if cfg.hist == nil {
			m.flash = "no history service mounted"
			return m, nil
		}
		m.inspecting = true
		m.refreshOverlay()
		m.overlay.GotoBottom()
		m.syncPalette() // the palette is inert under the inspector
		return m, nil
	}

	if key == "enter" && !m.inspecting {
		// A trailing backslash asks for a newline, not a send: the one
		// newline key that survives every terminal and the browser
		// (xterm.js sends shift+enter as a plain enter).
		if v := m.input.Value(); strings.HasSuffix(v, "\\") {
			m.setDraft(strings.TrimSuffix(v, "\\") + "\n")
			m.input.CursorEnd()
			return m, nil
		}
		line := strings.TrimSpace(m.input.Value())
		if line == "" {
			return m, nil
		}
		// A submitted "/" line NEVER reaches the LLM: it dispatches
		// through the commands service (absent service: plain text).
		if strings.HasPrefix(line, "/") && cfg.cmds != nil {
			return m, m.dispatch(line)
		}
		// A "!" line runs directly as a shell command — never the LLM.
		if strings.HasPrefix(line, "!") {
			return m, m.dispatchBang(line)
		}
		// A pending ask routes the submission as its ANSWER: a number
		// picks that option, anything else is freeform text.
		if m.pendingAsk != "" && m.answerPending(line) {
			return m, nil
		}
		m.input.Reset()
		return m, m.submit(line)
	}

	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette()
	m.layoutComposer()
	return m, cmd
}

// submit sends one line to the loop as user input, echoing it as a
// "user" block and starting the spinner.
func (m *model) submit(line string) tea.Cmd {
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "user", text: line, queued: m.running})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
	if !m.running {
		m.turnStart = time.Now()
	}
	m.running = true
	send := m.send
	return tea.Batch(
		func() tea.Msg { send(line); return nil },
		m.spin.Tick,
	)
}

// pane returns the scroll target: the overlay while inspecting,
// otherwise the transcript.
func (m *model) pane() *viewport.Model {
	if m.inspecting {
		return &m.overlay
	}
	return &m.vp
}

// refreshOverlay rebuilds the history overlay content, recording each
// entry's rendered line span for click hit-testing. Entries toggled
// open show their pretty-printed JSON inline.
func (m *model) refreshOverlay() {
	cfg := m.cfg.Load()
	if m.diving != 0 {
		for i := range m.blocks {
			if b := &m.blocks[i]; b.id == m.diving && b.sub != nil {
				off := m.overlay.YOffset()
				m.overlay.SetContent(m.subTranscript(b, cfg))
				m.overlay.SetYOffset(off)
				return
			}
		}
		m.diving = 0 // the card is gone (/clear): fall back to history
	}
	th := cfg.theme
	var sb strings.Builder
	sb.WriteString(th["accent"].Render("history") + " " + th["dim"].Render(cfg.hist.Path()) + "\n\n")
	line := 2
	m.ovRanges = m.ovRanges[:0]
	m.ovEntries = m.ovEntries[:0]
	entries := cfg.hist.Entries()
	for i, e := range entries {
		text, _ := e.Data["text"].(string)
		preview, _, _ := strings.Cut(text, "\n")
		if len(preview) > 80 {
			preview = preview[:80] + "…"
		}
		part := fmt.Sprintf("%s %s %s %s",
			th["dim"].Render(fmt.Sprintf("%4d", e.Seq)),
			th["dim"].Render(e.At.Format("15:04:05")),
			th["accent"].Render(fmt.Sprintf("%-9s", e.Kind)),
			preview)
		if m.ovExpanded[e.Seq] {
			js, err := json.MarshalIndent(e, "     ", "  ")
			if err != nil {
				js = fmt.Appendf(nil, "marshal: %v", err)
			}
			part += "\n     " + string(js)
		}
		n := strings.Count(part, "\n") + 1
		m.ovRanges = append(m.ovRanges, lineRange{start: line, end: line + n, idx: i})
		m.ovEntries = append(m.ovEntries, e.Seq)
		line += n
		sb.WriteString(part + "\n")
	}
	if len(entries) == 0 {
		sb.WriteString(th["dim"].Render("(no entries yet)"))
	}
	off := m.overlay.YOffset()
	m.overlay.SetContent(sb.String())
	m.overlay.SetYOffset(off)
}

func (m model) View() tea.View {
	v := tea.NewView(safeView(m.frame))
	v.AltScreen = true
	v.MouseMode = tea.MouseModeCellMotion // clicks toggle blocks; wheel scrolls
	return v
}

// frame renders the full-screen content; View wraps it in a panic guard.
func (m model) frame() string {
	cfg := m.cfg.Load()
	if m.picking {
		return m.pickerView(cfg)
	}
	if m.mp.open {
		return m.modelPickerView(cfg)
	}
	body := m.vp.View()
	if m.inspecting {
		body = m.overlay.View()
	} else if lines := m.overlayRows(); len(lines) > 0 {
		// The "/" palette: an overlay over the transcript's bottom
		// rows, directly above the composer — sized to its content,
		// never reflowing the layout under a filtering list.
		body = overlayBottom(body, lines)
	} else if m.todoPinned && m.todoText != "" {
		body = overlayBottom(body, m.todoPanel(cfg))
	}
	return body + "\n" + m.statusBar(cfg) + "\n" + m.input.View()
}

// overlayBottom replaces the bottom rows of body with lines (when
// they fit), the overlay pattern the palette and the todo panel share.
func overlayBottom(body string, lines []string) string {
	bl := strings.Split(body, "\n")
	if k := len(lines); len(bl) >= k {
		copy(bl[len(bl)-k:], lines)
		body = strings.Join(bl, "\n")
	}
	return body
}

// todoPanelMax caps the pinned todo panel's item rows.
const todoPanelMax = 8

// todoPanel renders the pinned todo list: a dim header naming the key
// that hides it, open items first as they are, done items dimmed,
// "+N more" past todoPanelMax. Pinned it tracks every todo event.
func (m *model) todoPanel(cfg *uiCfg) []string {
	th := cfg.theme
	items := strings.Split(strings.TrimRight(m.todoText, "\n"), "\n")
	lines := []string{th["dim"].Render("todo · " + cfg.keys["todo_toggle"] + " hides")}
	for i, l := range items {
		if i == todoPanelMax {
			lines = append(lines, th["dim"].Render(fmt.Sprintf("  +%d more", len(items)-i)))
			break
		}
		if strings.HasPrefix(l, "[x]") {
			l = th["dim"].Render(l)
		}
		lines = append(lines, xansi.Truncate(l, max(m.width, 1), "…"))
	}
	return lines
}
