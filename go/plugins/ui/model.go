package ui

import (
	"image/color"
	"slices"
	"time"

	xansi "github.com/charmbracelet/x/ansi"

	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
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

// eventsMsg is every loop event that was already queued when the
// reader woke: a fast stream lands as one batch and one render, not
// one full transcript render per delta (quadratic in reply length).
type eventsMsg []Event

// maxBatch caps one drain so a flood still yields to input and paint.
const maxBatch = 1024

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
	steer     bool     // user block sent INTO the running turn (see steerLine)
	pending   bool     // steer the loop has not landed yet (its next block boundary)
	files     []string // done blocks: files the turn wrote (from the entry's data)
	exit      *int     // done blocks: exit status, nil when absent

	// ask blocks only (see ask.go): the pending question's options and
	// id, and how it resolved.
	askID    string
	options  []string
	optRows  []int // pending render: option index per line, -1 = question
	answer   string
	answered bool
	expired  bool // turn ended (or replay found no answer entry) unanswered

	// spawn blocks only (see spawn.go): the subagent card's live state;
	// label is the task, text the child's report.
	sub  *subState
	live bool // assistant text still streaming (see addDelta)
}

// collapsible blocks get a disclosure header and can be toggled.
// closedByDefault is the "all" policy: every collapsible block starts
// closed — the transcript is the story, the detail is a click away.
func (m *model) closedByDefault(text string) bool {
	return m.cfg.Load().collapse == "all" && strings.Contains(text, "\n")
}

func (b *block) collapsible() bool {
	switch b.kind {
	case "code", "result", "thinking", "spawn", "error", "system", "todo", "job", "context", "memory":
		return true
	}
	return false
}

// lineRange maps a rendered line span [start, end) to a block index.
// fold marks the header row of an open fold led by that block.
type lineRange struct {
	start, end, idx int
	fold            bool
}

// model is the one transcript-plus-composer model used by tui and web.
type model struct {
	escHold      []tea.KeyPressMsg // Esc held to tell a key from a split report (escresidue.go)
	escGen       int
	escSince     time.Time
	escApplied   bool // the held Esc already ran (a running turn)
	vp           viewport.Model
	overlay      viewport.Model
	input        textarea.Model
	spin         spinner.Model
	events       <-chan Event
	deferRefresh bool // addEvent skips its render: more of the batch follows (eventsMsg)
	send         func(string)
	cfg          *atomic.Pointer[uiCfg]

	blocks      []block
	nextID      int
	focusID     int  // block-cursor identity; -1 when nothing focused
	focusFold   bool // the cursor is on the open-fold header above focusID, not the block
	ranges      []lineRange
	width       int
	height      int
	running     bool           // a turn is in flight (input sent, no done/error yet)
	turnStart   time.Time      // when the in-flight turn started (status bar elapsed)
	lastRequest time.Time      // when the model last answered (cache chip, cache.go)
	lastEnd     string         // how the last turn ended: "done", "cancelled", "" (tabtitle.go)
	inspecting  bool           // history overlay open
	diving      int            // spawn card id whose child transcript the overlay shows (0 = history)
	ovRanges    []lineRange    // overlay line span -> entry index
	ovExpanded  map[int64]bool // entry seq -> inline JSON shown
	ovEntries   []int64        // entry index -> seq, for ovRanges lookups
	picking     bool           // session picker shown instead of the chat view
	pick        int            // picker cursor index into cfg.sessions
	mp          modelPicker    // "/model" picker (see modelpick.go)
	rw          rewindPicker   // double-esc rewind menu (see rewind.go)
	srch        searchBar      // ctrl+s transcript search (see search.go)
	todoText    string         // latest todo list text (the todo plugin's event)
	title       string         // the session's name (session-title plugin); "" until named
	sessID      string         // the session the pane last replayed
	activity    string         // what the agent is doing now (activity plugin); "" when idle
	pred        predictState   // the small model's guess at the rest of the draft (predict.go)
	todoHidden  bool           // the todo strip dismissed for now (todo_toggle)
	board       boardState     // the attention board at the top (board.go)
	sessRows    sessList       // mid-session picker list (see session.go); nil = launch picker
	welcome     bool           // fresh-session orientation text (see welcomeView)
	unfolded    map[int]int    // fold lead id -> id of its run's last block, shown as rows (see fold.go)
	keepRow     map[int]bool   // block ids closed by hand: they stay rows, never fold (see fold.go)
	pendingAsk  string         // ask id the composer routes answers to; "" = none
	keysBlock   int            // id of the last "?"/keys block; esc drops it first while an ask is pending
	pal         palette        // "/" command palette (see palette.go)
	at          palette        // "@" file picker (see atfiles.go)
	atFiles     []string       // the picker's file list, read when it opens
	atCapped    bool           // the unfiltered walk hit atMaxFiles: re-walk per query
	atWalkQ     string         // the query atFiles was walked for
	flash       string
	v           voiceState    // voice dictation (voice.go)
	trailing    string        // assistant prose after an executed fence, emitted after its result
	newBelow    bool          // blocks arrived while scrolled up (status cue)
	sel         selection     // mouse drag selection (see select.go)
	lines       []string      // rendered content lines, for the selection
	stop        stopState     // quit-key arming (see stop.go)
	bang        *bangRun      // the running "!" command (esc cancels); nil = none
	leader      bool          // the leader key was pressed: the next key is a chord (see actions.go)
	comp        composerState // prompt recall (see composer.go)
	tab         tabState      // Tab path-completion cycling (see pathcomplete.go)
	md          *glamour.TermRenderer
	mdCache     map[string]string // assistant markdown render cache (cleared on resize)
	parts       map[int]partEntry // per-block rendered part, by block id (cleared with mdCache)
	liveHead    liveWrap          // the streaming reply's wrapped finished lines (render)
	bgLight     bool              // terminal background is light (tea.BackgroundColorMsg)
	sized       bool              // a real WindowSizeMsg arrived (newModel's size is a placeholder)
}

// partEntry is one block's fitted render and what it was rendered
// from: refresh reuses it while the block, the theme and the focus are
// unchanged, so a long transcript is not re-laid-out on every event.
type partEntry struct {
	b       block
	cfg     *uiCfg
	focused bool
	part    string
}

// sameBlock reports whether two block values render the same.
func sameBlock(a, b block) bool {
	return a.id == b.id && a.kind == b.kind && a.text == b.text && a.label == b.label &&
		a.collapsed == b.collapsed && a.queued == b.queued && a.steer == b.steer &&
		a.pending == b.pending && slices.Equal(a.files, b.files) &&
		(a.exit == nil) == (b.exit == nil) && (a.exit == nil || *a.exit == *b.exit) &&
		a.askID == b.askID && slices.Equal(a.options, b.options) && a.answer == b.answer &&
		a.answered == b.answered && a.expired == b.expired && a.sub == b.sub && a.live == b.live
}

// renderPart is render + fit, cached per block. Spawn cards (spinner,
// clock) and streaming blocks change without their fields changing,
// so they always render fresh.
func (m *model) renderPart(b *block, cfg *uiCfg) string {
	cacheable := b.kind != "spawn" && !b.live
	if e, ok := m.parts[b.id]; cacheable && ok && e.cfg == cfg && e.focused == m.focused(b) && sameBlock(e.b, *b) {
		return e.part
	}
	part := m.fit(strings.Trim(squeezeBlanks(m.render(b, cfg)), "\n"))
	if cacheable {
		m.parts[b.id] = partEntry{b: *b, cfg: cfg, focused: m.focused(b), part: part}
	}
	return part
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
		focusID: -1, ovExpanded: map[int64]bool{}, mdCache: map[string]string{}, parts: map[int]partEntry{}, comp: composerState{recall: -1}}
	m.resize(width, height)
	if b := cfg.Load().board; b != nil && b.Sticky() {
		m.board.on = true
	}
	if d := cfg.Load().draft; d != "" {
		// The composer opens with the text; the person finishes it.
		m.input.SetValue(d)
		m.input.CursorEnd()
	}
	if c := cfg.Load(); c.voiceMode != "" && c.voice != nil && recorderTool() != "" {
		m.v.mode = c.voiceMode
	}
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
	// Read before the height changes: a shrink turns the old bottom
	// offset into a mid-transcript one, and refresh would keep it.
	atBottom := m.vp.AtBottom()
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
	m.parts = map[int]partEntry{}
	m.refresh()
	if atBottom {
		m.vp.GotoBottom()
	}
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
	parts := make([]string, 0, 2*len(m.blocks))
	m.ranges = m.ranges[:0]
	start, prev := 0, ""
	next := map[int]foldRun{}
	for _, r := range m.runs() {
		next[r.from] = r
	}
	skipTo := 0
	rule := cfg.theme["border"].Render(strings.Repeat("─", max(m.width, 1)))
	for i := range m.blocks {
		if i < skipTo {
			continue
		}
		// The block renders own their content; the transcript owns the
		// space between them: nothing between blocks of the same voice,
		// one rule where the voice changes.
		part := m.renderPart(&m.blocks[i], cfg)
		voice := voiceOf(m.blocks[i].kind)
		var header string
		if r, ok := next[i]; ok {
			if r.open {
				// An open run keeps its row as a header above its
				// blocks: the way back to one line.
				header = m.fit(m.renderFold(r, cfg.theme))
			} else {
				// A folded run draws as one row owned by its lead block,
				// so a click or the block cursor lands on the fold, not
				// on a step the reader cannot see.
				part = m.fit(m.renderFold(r, cfg.theme))
				skipTo = r.to
			}
		}
		if i > 0 && separates(prev, voice) {
			parts = append(parts, rule)
			start++
		}
		if header != "" {
			m.ranges = append(m.ranges, lineRange{start: start, end: start + 1, idx: i, fold: true})
			start++
			parts = append(parts, header)
		}
		n := strings.Count(part, "\n") + 1
		m.ranges = append(m.ranges, lineRange{start: start, end: start + n, idx: i})
		start += n
		parts = append(parts, part)
		prev = voice
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
	// glamour pads inline code with a non-breaking space on each side,
	// to keep a span from breaking across lines. The cost shows up in
	// every reply that names a symbol: "Fixed to  a + b ;  go run .
	// now prints  5 ." — a double space before each span and a gap
	// before the punctuation after it. The background colour is what
	// separates code from prose here, so the padding buys nothing.
	cfg.Code.Prefix, cfg.Code.Suffix = "", ""
	accent := hexOf(th["accent"].GetForeground())
	if accent != "" {
		cfg.Code.Color = &accent
	}
	if bg := hexOf(th["status"].GetBackground()); bg != "" {
		cfg.Code.BackgroundColor = &bg
	}
	// Headings: the stock style keeps the "## " markers, so a heading
	// reads as unformatted markdown. Drop them and make the levels
	// tell apart by weight: H1 a filled bar, H2 bold underlined, H3
	// bold, H4+ bold and dim.
	yes, no := true, false
	if accent != "" {
		cfg.Heading.Color = &accent
	}
	cfg.Heading.Bold = &yes
	cfg.H1.Prefix, cfg.H1.Suffix = " ", " "
	cfg.H2.Prefix, cfg.H2.Suffix = "", ""
	cfg.H2.Underline = &yes
	cfg.H3.Prefix, cfg.H3.Suffix = "", ""
	for _, h := range []*ansi.StyleBlock{&cfg.H4, &cfg.H5, &cfg.H6} {
		h.Prefix, h.Suffix = "", ""
		h.Faint = &yes
		h.Bold = &no
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
// voiceOf groups a block by who is speaking: you, the model, or the
// machinery it drove. A rule is drawn where the voice changes.
func voiceOf(kind string) string {
	switch kind {
	case "user", "steer":
		return "user"
	case "assistant":
		return "assistant"
	case "done":
		return "done" // its own divider; never doubled with a rule
	}
	return "tool"
}

// separates reports whether a rule belongs between two voices. The turn
// marker draws its own, so nothing is added on either side of it.
func separates(prev, cur string) bool {
	return prev != cur && prev != "done" && cur != "done"
}

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
		if b.live {
			tag = "thinking…" // still arriving; the count grows as it does
		}
	case "error", "system", "todo", "job", "context":
		tag = b.kind
	case "memory":
		tag = "◆ remembered"
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
	// A todo receipt shows no preview: the live list is pinned above
	// the composer, and repeating its first line here prints the same
	// item twice on one screen.
	if b.kind == "todo" {
		st := th["dim"]
		if m.focused(b) {
			st = th["focus"]
		}
		return st.Render(head)
	}
	if b.kind != "code" || tag == "code js" {
		head += ": " + strings.SplitN(b.text, "\n", 2)[0]
	}
	if r := []rune(head); len(r) > m.width-1 && m.width > 2 {
		head = string(r[:m.width-2]) + "…"
	}
	st := th["dim"]
	if m.focused(b) {
		st = th["focus"]
	}
	return st.Render(head)
}

// welcomeView is the fresh-session orientation text (tui/web only):
// shown while the transcript is empty on a session with no history,
// suppressed on resume, gone once anything renders, removed by /clear.
// Under the title, one dim line each for what is loaded (context
// files, skills, templates — omitted when empty) and the keys to
// know; every line is clipped to the width.
func (m *model) welcomeView(cfg *uiCfg) string {
	th := cfg.theme
	lines := welcomeLines(cfg)
	out := th["accent"].Render("● ") + th["dim"].Render("bough — a coding agent")
	for _, l := range lines {
		out += "\n" + th["dim"].Render(truncateCols("  "+l, max(m.width, 4)))
	}
	// The mark sits in the empty pane under the header, centred in
	// what is left (see splash.go); a short pane gets the text alone.
	used := len(lines) + 1
	if art := mark(m.width, m.vp.Height()-used, 2); art != nil {
		top := (m.vp.Height() - used - len(art)) / 2
		out += strings.Repeat("\n", max(top, 1)) + strings.Join(art, "\n")
	}
	return out
}

// welcomeNames caps the skill names the header spells out.
const welcomeNames = 5

// welcomeLines is the header body: "model <name> · <n> context",
// "context: <files>", "skills: N
// (names…)", "templates: /a /b", each omitted when there is nothing,
// then the keys line and the invitation.
func welcomeLines(cfg *uiCfg) []string {
	var out []string
	// The startup line: which model answers and, when known, how much
	// context it has.
	if cfg.modeler != nil && cfg.modeler.Model() != "" {
		line := "model " + cfg.modeler.Model()
		if cfg.limit != nil && cfg.limit.ContextLimit() > 0 {
			line += " · " + ctxAbbrev(cfg.limit.ContextLimit()) + " context"
		}
		out = append(out, line)
	}
	if cfg.ctxmd != nil {
		if files := cfg.ctxmd.Loaded(); len(files) > 0 {
			out = append(out, "context: "+strings.Join(tildePaths(files), ", "))
		}
	}
	if cfg.skills != nil {
		if names := cfg.skills.Names(); len(names) > 0 {
			shown := names
			if len(shown) > welcomeNames {
				shown = append(shown[:welcomeNames:welcomeNames], "…")
			}
			out = append(out, fmt.Sprintf("skills: %d (%s)", len(names), strings.Join(shown, ", ")))
		}
	}
	if cfg.cmds != nil {
		var names []string
		for _, in := range cfg.cmds.List() {
			if in.IsTemplate() {
				names = append(names, "/"+in.Name)
			}
		}
		if len(names) > 0 {
			out = append(out, "templates: "+strings.Join(names, " "))
		}
	}
	return append(out,
		"keys: ? for the list · / for commands · ! for shell",
		"ask me to do something — I act by running code")
}

// tildePaths shortens paths under the home directory to "~/…".
func tildePaths(paths []string) []string {
	home, _ := os.UserHomeDir()
	out := make([]string, len(paths))
	for i, p := range paths {
		if rest, ok := strings.CutPrefix(p, home+string(filepath.Separator)); ok && home != "" {
			p = "~/" + rest
		}
		out[i] = p
	}
	return out
}

// authErrRe spots credential-shaped failures in error text; the match
// appends the credential hint below the error block.
var authErrRe = regexp.MustCompile(`(?i)\b40[13]\b|unauthorized|credentials|api[ _-]?key|x-api-key`)

const authHint = "hint: check your provider credentials (ANTHROPIC_API_KEY / OPENROUTER_API_KEY), or swap the llm row — /model"

// liveWrap caches the wrapped render of a live reply's finished lines.
type liveWrap struct {
	width     int
	cfg       *uiCfg
	head, out string
}

// wrapLive wraps streaming prose plus its cursor. Lines wrap on their
// own, so the finished lines render once and only the last line is
// wrapped per frame: re-wrapping the whole reply on every render made
// a long stream quadratic and the screen trailed the turn by minutes.
func (m *model) wrapLive(prose string, st lipgloss.Style, cfg *uiCfg) string {
	i := strings.LastIndexByte(prose, '\n')
	if i < 0 {
		return st.Width(m.width).Render(prose + "▌")
	}
	head, tail := prose[:i], prose[i+1:]
	c := &m.liveHead
	switch {
	case c.width == m.width && c.cfg == cfg && c.head == head:
	case c.width == m.width && c.cfg == cfg && c.head != "" &&
		strings.HasPrefix(head, c.head) && head[len(c.head)] == '\n':
		// More finished lines: wrap only the new ones.
		c.out += "\n" + st.Width(m.width).Render(head[len(c.head)+1:])
		c.head = head
	default:
		*c = liveWrap{width: m.width, cfg: cfg, head: head, out: st.Width(m.width).Render(head)}
	}
	return c.out + "\n" + st.Width(m.width).Render(tail+"▌")
}

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
			// Streaming shows only settled prose, with a cursor;
			// markdown waits for the finished reply. Anything whose
			// meaning is not yet known is held back and stands in as a
			// dim note: a code fence (it becomes a block), a thinking
			// span (it becomes a collapsed block), a fabricated
			// <system-…> message (it is removed). Typing text out and
			// then taking it away is worse than a moment of nothing.
			prose, coding, thinking := liveView(b.text)
			out := head
			if prose != "" || !(coding || thinking) {
				out += "\n" + m.wrapLive(prose, th["assistant"], cfg)
			}
			if thinking {
				out += "\n" + th["dim"].Render("▸ thinking…")
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
	case "memory":
		// What was written down after the turn, for later sessions.
		// A receipt, not the work, so it is dim; but it is the one
		// thing here the user did not watch happen, so each fact
		// carries the ◆ mark that says "this was remembered".
		if b.collapsed {
			return m.header(b, th)
		}
		var lines []string
		for _, l := range strings.Split(b.text, "\n") {
			if l == "" || strings.HasPrefix(l, "(") || strings.HasPrefix(l, "memory:") {
				lines = append(lines, th["system"].Render(l))
				continue
			}
			lines = append(lines, th["accent"].Render("◆ ")+th["system"].Render(l))
		}
		return lipgloss.NewStyle().Width(max(m.width, 10)).Render(strings.Join(lines, "\n"))
	case "context":
		// Text the model was given that the user never typed: an
		// AGENTS.md, a skill a word in the message matched, a hook's
		// addition. Collapsed to a one-line header; open it to read
		// exactly what went in.
		if b.collapsed {
			return m.header(b, th)
		}
		return th["system"].Width(max(m.width, 10)).Render(b.text)
	case "job":
		// A background job reported back. Dim like other machinery,
		// but it is news the user did not ask for, so it says so.
		if b.collapsed {
			return m.header(b, th)
		}
		return th["system"].Width(max(m.width, 10)).Render(b.text)
	case "system":
		// Dimmed command output, wrapped to width so /help rows never
		// clip off the right edge; a collapsed one is a header row.
		if b.collapsed {
			return m.header(b, th)
		}
		return linkURLs(th["system"].Width(max(m.width, 10)).Render(b.text))
	case "error":
		// Wrap to width — the viewport clips long lines, and the tail
		// of an error is usually the actionable part. Collapsed, the
		// first line stays visible: an error you cannot read is worse
		// than a row of noise.
		w := max(m.width, 10)
		text := b.text
		glyph := ""
		if b.collapsible() && strings.Contains(b.text, "\n") {
			glyph = "▾ "
			if b.collapsed {
				glyph, text = "▸ ", strings.SplitN(b.text, "\n", 2)[0]+" …"
			}
		}
		var out string
		if b.collapsed {
			// One row, like every other closed block: truncate, never wrap.
			out = xansi.Truncate(th["error"].Render(glyph+"✗ "+text), m.width, "…")
		} else {
			out = th["error"].Width(w).Render(glyph + "✗ " + text)
		}
		// The generic hint is for errors that only say something was
		// rejected. An error that already names the fix (llm.MissingKey
		// spells out ~/.bough/env and /model) does not need it repeated
		// underneath in shorter words.
		if authErrRe.MatchString(b.text) && !strings.Contains(b.text, "~/.bough/env") {
			out += "\n" + th["dim"].Width(w).Render(authHint)
		}
		return out
	case "todo":
		// Dedicated todo render: dim tag + checkbox lines, done items
		// dimmed. See addEvent for the one-render-per-mutation rule.
		if b.collapsed {
			return m.header(b, th)
		}
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

// box renders text in a rounded border. The text is hard-wrapped to
// the box first: lipgloss only breaks at spaces, so one long unbroken
// run (a rule of dashes, a URL, a hash) would widen the box past the
// pane and hand the whole transcript a sideways scroll.
func (m *model) box(text string, content, border lipgloss.Style) string {
	w := max(m.width-4, 10)
	text = xansi.Hardwrap(strings.TrimRight(text, "\n"), w-4, true) // w less border and padding
	return content.
		Border(lipgloss.RoundedBorder()).
		BorderForeground(border.GetForeground()).
		Padding(0, 1).
		Width(w).
		Render(text)
}

// fit is the last line of defence against a sideways-scrolling pane:
// whatever a render produced, no line leaves wider than the pane.
func (m *model) fit(s string) string {
	if m.width < 2 {
		return s
	}
	return xansi.Hardwrap(s, m.width, true)
}

// waitEvent blocks for the next loop event.
func (m model) waitEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.events
		if !ok {
			return nil
		}
		batch := []Event{ev}
		for len(batch) < maxBatch {
			select {
			case ev, ok := <-m.events:
				if !ok {
					return eventsMsg(batch)
				}
				batch = append(batch, ev)
				continue
			default:
			}
			break
		}
		if len(batch) == 1 {
			return eventMsg(ev)
		}
		return eventsMsg(batch)
	}
}

func (m model) Init() tea.Cmd {
	cmds := []tea.Cmd{m.waitEvent(), tea.RequestBackgroundColor, m.cacheTick()}
	if m.board.on {
		cmds = append(cmds, m.loadBoard(m.cfg.Load()), m.spin.Tick)
	}
	return tea.Batch(cmds...)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		changed := m.sized && (m.width != msg.Width || m.height != msg.Height)
		m.sized = true
		m.resize(msg.Width, msg.Height)
		if !changed {
			return m, nil // first size, or no change: nothing drawn at another size
		}
		// A frame the renderer drew for the old size can reach the
		// terminal after it resized, and its scroll optimisation leaves
		// margins set to the old height (ESC[1;12r). The renderer's
		// full redraw never resets them, so a taller screen redraws
		// inside the old region and stays torn. Reset the margins, then
		// repaint the whole screen.
		return m, tea.Sequence(tea.Raw("\x1b[r"), tea.ClearScreen)

	case boardMsg:
		m.takeBoard(msg.b)
		if !m.board.on {
			return m, nil
		}
		return m, boardTick()

	case hoverMsg:
		m.takeHover(msg)
		return m, nil

	case boardTickMsg:
		if !m.board.on {
			return m, nil
		}
		return m, m.loadBoard(m.cfg.Load())

	case spinner.TickMsg:
		// The spinner drives two things: the running turn and the
		// background-job strip (which outlives the turn). Either keeps
		// it ticking.
		if !m.running && len(m.jobRows(m.cfg.Load())) == 0 {
			return m, nil
		}
		var cmd tea.Cmd
		m.spin, cmd = m.spin.Update(msg)
		m.layoutComposer() // a job that just started or ended resizes the pane
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
		if Event(msg).Kind == "done" {
			return m, tea.Batch(m.waitEvent(), m.cacheTick())
		}
		return m, m.waitEvent()

	case eventsMsg:
		done := false
		for i, ev := range msg {
			// A delta followed by more events skips its render: the
			// last event of the batch renders the lot.
			m.deferRefresh = i < len(msg)-1 && strings.HasSuffix(ev.Kind, "-delta")
			m.addEvent(ev)
			done = done || ev.Kind == "done"
		}
		m.deferRefresh = false
		if done {
			return m, tea.Batch(m.waitEvent(), m.cacheTick())
		}
		return m, m.waitEvent()

	case cacheTickMsg:
		return m, nil // the bar re-reads the clock on every draw

	case predictTickMsg:
		return m, m.startPredict(m.cfg.Load(), msg.draft)

	case predictMsg:
		m.finishPredict(msg)
		return m, nil

	case bangDoneMsg:
		m.finishBang(msg)
		return m, nil

	case bangTickMsg:
		return m, m.tickBang(msg)

	case editorDoneMsg:
		m.finishEditor(msg)
		return m, nil

	case imagePasteMsg:
		return m, m.finishPaste(msg)

	case tea.BackgroundColorMsg:
		// Re-render markdown for the actual terminal background so
		// dark-style grays never land on a light terminal.
		if light := !msg.IsDark(); light != m.bgLight {
			m.bgLight = light
			m.md = nil
			m.mdCache = map[string]string{}
			m.parts = map[int]partEntry{}
			m.refresh()
		}
		return m, nil

	case tea.KeyPressMsg:
		if took, cmd := m.escFilter(msg); took {
			return m, cmd
		}
		return m.handleKey(msg)

	case escHoldMsg:
		if msg.gen != m.escGen || m.escHold == nil {
			return m, nil
		}
		if time.Since(msg.at) > escLate && time.Since(m.escSince) < escHoldMax {
			return m, m.escTimer()
		}
		return m, m.escRelease()

	case tea.MouseClickMsg:
		if m.leader {
			m.leader, m.flash = false, "" // a click is not a chord: the pending leader lapses
		}
		// The board sits above the transcript: its rows take the
		// event, or the event moves up past them.
		cfg := m.cfg.Load()
		if took, cmd := m.boardMouse(cfg, msg); took {
			return m, cmd
		}
		return m, m.handleClick(shiftMouse(msg.Mouse(), m.boardHeight(cfg)))

	case tea.MouseMotionMsg:
		cfg := m.cfg.Load()
		if took, cmd := m.boardMouse(cfg, msg); took {
			return m, cmd // the hover's detail fetch
		}
		m.dragSelect(shiftMouse(msg.Mouse(), m.boardHeight(cfg)))
		return m, nil // never the composer's business

	case tea.MouseReleaseMsg:
		cfg := m.cfg.Load()
		if took, cmd := m.boardMouse(cfg, msg); took {
			return m, cmd
		}
		return m, m.releaseSelect(shiftMouse(msg.Mouse(), m.boardHeight(cfg)))

	case copiedMsg:
		m.finishCopy(msg)
		return m, nil

	case voiceMsg:
		return m.finishVoice(msg)

	case voiceTickMsg:
		return m, m.voiceTicked(m.cfg.Load())

	case tea.KeyReleaseMsg:
		return m, m.voiceRelease(msg, m.cfg.Load())

	case tea.PasteMsg:
		m.stop.armedAt = time.Time{} // a paste is typing: it disarms quit like any key
		m.stop.escAt = time.Time{}
		if m.mp.open {
			// The picker owns the keyboard; a paste is search text,
			// not a hidden edit to the composer draft behind it.
			m.mp.query += strings.Join(strings.Fields(msg.Content), " ")
			m.mp.filter()
			return m, nil
		}
		if m.picking {
			return m, nil // the session picker has no text field: drop it, don't fill the hidden draft
		}
		if took, cmd := m.handlePaste(msg); took {
			return m, cmd
		}
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
		m.lastRequest = time.Now()
		if m.lastEnd != "cancelled" {
			m.lastEnd = "done" // the cancelled marker came first when it did
		}
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
		m.blocks = append(m.blocks, block{id: id, kind: "error", text: errorText(ev.Text), collapsed: m.closedByDefault(errorText(ev.Text))})
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
	case "activity":
		// What the agent is doing right now, for the status line: the
		// transcript already says what it did. A label that arrives
		// after the turn ended is dropped, or it would caption the
		// NEXT turn's first seconds with the last one's work.
		if !m.running && ev.Text != "" {
			return
		}
		m.activity = ev.Text
		m.refresh()
		return
	case "title":
		// The session's name belongs in the bar, not the transcript:
		// it is what this conversation IS, not something that happened
		// in it.
		// One line, no escapes or bidi overrides: it lands in the
		// status bar and in the OSC 2 tab title.
		m.title = strings.Join(strings.Fields(strings.Map(func(r rune) rune {
			if (r >= 0x202a && r <= 0x202e) || (r >= 0x2066 && r <= 0x2069) || (r >= 0x80 && r < 0xa0) {
				return -1
			}
			return r
		}, sanitizeText(ev.Text))), " ")
		m.refresh()
		return
	case "thinking-delta":
		m.addThinkDelta(id, ev.Text)
	case "thinking":
		// The finished reasoning replaces what streamed: same block,
		// same collapsed state, no second copy.
		m.finishThinking(id, ev.Text)
	case "assistant-delta":
		m.addDelta(id, ev.Text)
	case "assistant":
		m.dropLive()
		m.addAssistant(ev.Text)
	case "steer":
		m.landSteer(id, ev.Text)
	case "todo":
		m.todoText = ev.Text
		// One render per mutation: the system block a /todo mutation
		// just printed (same text) becomes the todo block, and
		// back-to-back todo events (several mutations in one script)
		// update one block instead of stacking copies.
		if n := len(m.blocks); n > 0 {
			if last := &m.blocks[n-1]; last.kind == "todo" ||
				(last.kind == "system" && last.text == ev.Text) {
				last.kind, last.text, last.collapsed = "todo", ev.Text, true
				m.refresh()
				m.vp.GotoBottom()
				return
			}
		}
		// The live list is pinned above the composer now, so the
		// transcript keeps only a receipt of the change: an open copy
		// here would print the same list twice, a few rows apart.
		m.blocks = append(m.blocks, block{id: id, kind: "todo", text: ev.Text, collapsed: true})
	case "system":
		// A note that supersedes the reply above it (the loop asking
		// again for a stop block): drop that reply, or the user reads
		// the same answer twice.
		if ev.Data["supersedes"] == true {
			for i := len(m.blocks) - 1; i >= 0; i-- {
				if m.blocks[i].kind == "assistant" {
					m.blocks = append(m.blocks[:i], m.blocks[i+1:]...)
					break
				}
			}
		}
		m.blocks = append(m.blocks, block{id: id, kind: "system", text: ev.Text,
			collapsed: m.closedByDefault(ev.Text)})
	default: // assistant, anything future
		if ev.Kind == "cancelled" {
			m.lastEnd = "cancelled" // the done that follows keeps it (tabtitle.go)
		}
		// A loop-event system note is detail; command output (slash.go)
		// is what the user asked for and stays open.
		m.blocks = append(m.blocks, block{id: id, kind: ev.Kind, text: ev.Text,
			collapsed: (ev.Kind == "system" || ev.Kind == "job" || ev.Kind == "context" || ev.Kind == "memory") &&
				m.closedByDefault(ev.Text)})
	}
	if m.deferRefresh {
		return
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
			if r.fold {
				m.refold(r.idx)
				return nil
			}
			b := &m.blocks[r.idx]
			if b.kind == "ask" && b.askID == m.pendingAsk && !b.answered && !b.expired {
				// Wrapped option rows map back through optRows.
				if off := row - r.start; off >= 0 && off < len(b.optRows) && b.optRows[off] >= 0 {
					m.answerAsk(b, b.options[b.optRows[off]])
				}
				return nil
			}
			if b.collapsible() || m.closedLead(r.idx) {
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

// stop is one block-cursor position: a block, or (fold) the header
// row of the open fold that block leads.
type stop struct {
	idx  int
	fold bool
}

// focusables returns the block-cursor stops, in order: the collapsible
// blocks, with an open fold's header just before its lead.
func (m *model) focusables() []stop {
	hidden := map[int]bool{}
	open := map[int]bool{}
	closed := map[int]bool{}
	for _, r := range m.runs() {
		if r.open {
			open[r.lead] = true
			continue
		}
		closed[r.lead] = true
		for i := r.from + 1; i < r.to; i++ {
			hidden[i] = true // drawn as part of the lead's fold row
		}
	}
	var out []stop
	for i := range m.blocks {
		if open[i] {
			out = append(out, stop{idx: i, fold: true})
		}
		if (m.blocks[i].collapsible() || closed[i]) && !hidden[i] {
			out = append(out, stop{idx: i})
		}
	}
	return out
}

// focused reports whether block b holds the cursor (not its fold header).
func (m *model) focused(b *block) bool {
	return b.id == m.focusID && !m.focusFold
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
	for i, s := range f {
		if m.blocks[s.idx].id == m.focusID && s.fold == m.focusFold {
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
	m.focusID = m.blocks[f[next].idx].id
	m.focusFold = f[next].fold
	m.refresh()
	for _, r := range m.ranges {
		if r.idx == f[next].idx && r.fold == f[next].fold {
			m.vp.EnsureVisible(r.start, 0, 0)
			break
		}
	}
}

// toggleFocused flips the focused block's collapsed state; false when
// nothing is focused.
func (m *model) toggleFocused() bool {
	for i := range m.blocks {
		if m.blocks[i].id != m.focusID {
			continue
		}
		if m.focusFold {
			m.refold(i)
			return true
		}
		if m.blocks[i].collapsible() || m.closedLead(i) {
			m.toggleBlock(i)
			return true
		}
	}
	return false
}

// closedLead reports whether block i leads a closed fold: a one-line
// narration can lead one without being collapsible itself.
func (m *model) closedLead(i int) bool {
	r, ok := m.foldAt(i)
	return ok && !r.open
}

// toggleBlock flips block i, focuses it, and keeps its header on
// screen: with the transcript pinned to the bottom, expanding a long
// block used to scroll the header you just clicked out of view.
func (m *model) toggleBlock(i int) {
	// The lead of a folded run answers for the whole run: opening it
	// puts the steps back as rows, each still closed.
	if r, ok := m.foldAt(i); ok && !r.open {
		m.unfold(i)
		return
	}
	m.blocks[i].collapsed = !m.blocks[i].collapsed
	// Closing a block by hand keeps it a row: folded into a neighbour's
	// run it would vanish, and enter again could not reopen it.
	if m.blocks[i].collapsed {
		if m.keepRow == nil {
			m.keepRow = map[int]bool{}
		}
		m.keepRow[m.blocks[i].id] = true
	} else {
		delete(m.keepRow, m.blocks[i].id)
	}
	m.focusID = m.blocks[i].id
	m.focusFold = false
	m.refresh()
	for _, r := range m.ranges {
		if r.idx == i && !r.fold {
			m.vp.EnsureVisible(r.start, 0, 0)
			break
		}
	}
}

// setAllCollapsed collapses or expands every collapsible block,
// returning how many changed. Expanding skips blocks over previewCap
// lines unless focused (see blocks.go).
func (m *model) setAllCollapsed(collapsed bool) int {
	m.keepRow = nil
	n := 0
	for i := range m.blocks {
		if b := &m.blocks[i]; b.collapsible() && b.collapsed != collapsed && m.mayExpand(b, collapsed) {
			b.collapsed = collapsed
			n++
		}
	}
	// An open step fold is state of its own: collapsing everything
	// closes it back to one row too.
	if collapsed && len(m.unfolded) > 0 {
		n += len(m.unfolded)
		clear(m.unfolded)
		m.focusFold = false
	}
	m.refresh()
	return n
}

// handleKey resolves every binding through the keymap service; only
// enter (submit — not a remappable action) is fixed. Enter first acts
// as collapse_toggle when a block is focused, and submits otherwise.
// The leader key takes the key after it as a chord (see actions.go).
func (m model) handleKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	if m.picking {
		return m.handlePickerKey(msg)
	}
	if m.mp.open {
		return m.handleModelPickerKey(msg)
	}
	if m.rw.open {
		return m.handleRewindKey(msg)
	}
	if m.srch.open {
		return m.handleSearchKey(msg)
	}
	cfg := m.cfg.Load()
	m.flash = ""
	key := msg.String()
	if m.leader {
		m.leader = false
		return m, m.chordKey(key, cfg)
	}
	if cfg.action[key] == "leader" {
		// Pending: the bar says so, and the quit stays armed across it
		// (ctrl+x q twice must quit like ctrl+c twice).
		m.leader = true
		m.flash = key + " …"
		return m, nil
	}
	if cfg.action[key] != "quit" {
		m.stop.armedAt = time.Time{} // any other key disarms, whichever handler takes it
	}
	if key != "esc" {
		m.stop.escAt = time.Time{}
	}

	// The palette owns Up/Down/Tab/Enter/Esc while it is open, and
	// nothing else: what it passes on falls through to the keymap
	// waterfall and the composer (which re-filters). follow_up
	// (alt+enter) accepts the selection like enter: it never submits
	// a half-typed "/mo" past the open list.
	pkey := key
	if cfg.action[key] == "follow_up" {
		pkey = "enter"
	}
	if m.pal.open && !m.inspecting {
		if handled, cmd := m.paletteKey(pkey); handled {
			return m, cmd
		}
	}
	if m.at.open && !m.inspecting {
		if handled, cmd := m.atKey(pkey); handled {
			return m, cmd
		}
	}

	// esc closes a subagent dive (the history inspector keeps its own key).
	if key == "esc" && m.inspecting && m.diving != 0 {
		m.inspecting, m.diving = false, 0
		m.syncPalette()
		return m, nil
	}

	// Help shown over a pending ask closes first: esc drops the keymap
	// block so the ask's options are back in view, still pending.
	if key == "esc" && m.pendingAsk != "" && !m.inspecting && m.keysBlock != 0 &&
		len(m.blocks) > 0 && m.blocks[len(m.blocks)-1].id == m.keysBlock {
		m.blocks = m.blocks[:len(m.blocks)-1]
		m.keysBlock = 0
		m.refresh()
		m.vp.GotoBottom()
		return m, nil
	}

	// A pending ask owns esc: decline it (the literal "(declined)" is
	// the tool's return value, so the model knows it was waved off).
	if key == "esc" && m.pendingAsk != "" && !m.inspecting {
		m.answerPending("(declined)")
		return m, nil
	}

	// A running "!" command owns esc: cancel it (kills its group).
	if key == "esc" && m.bang != nil && !m.inspecting {
		m.bang.cancel()
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
	if handled, cmd := m.voiceKey(key, msg, cfg); handled {
		return m, cmd
	}
	// ctrl+v probes the clipboard for an image first (imagepaste.go);
	// a text clipboard replays the key into the textarea. A keymap
	// binding on ctrl+v takes it instead.
	if key == "ctrl+v" && !m.inspecting && cfg.action[key] == "" {
		return m, m.pasteKey(msg)
	}
	if handled, cmd := m.composerKey(key, msg); handled {
		return m, cmd
	}

	action := cfg.action[key]
	if action == "collapse_toggle" && key == "enter" {
		// Enter toggles only a focused block on an empty composer;
		// composing, or with nothing focused, it submits below.
		if strings.TrimSpace(m.input.Value()) == "" && !m.inspecting && m.toggleFocused() {
			return m, nil
		}
		action = ""
	}
	if action != "" && action != "follow_up" { // follow_up is enter's twin below
		return m, m.runAction(action, key, cfg)
	}

	// follow_up (alt+enter) is enter that always queues: while a turn
	// runs and the loop takes steers, plain enter steers it instead.
	followUp := cfg.action[key] == "follow_up"
	if (key == "enter" || followUp) && !m.inspecting {
		// A trailing backslash asks for a newline, not a send: the one
		// newline key that survives every terminal and the browser
		// (xterm.js sends shift+enter as a plain enter).
		if v := m.input.Value(); strings.HasSuffix(v, "\\") {
			m.setDraft(strings.TrimSuffix(v, "\\") + "\n")
			m.input.CursorEnd()
			return m, nil
		}
		// An attached image deleted since the paste would be dropped
		// silently downstream: keep the draft and say so instead.
		if p := m.missingImage(m.input.Value()); p != "" {
			m.flash = "image missing: " + p + " · delete the tag to drop it"
			return m, nil
		}
		line := strings.TrimSpace(m.expandPastes(m.input.Value()))
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
		if m.running && cfg.steer != nil && !followUp {
			return m, m.steerLine(line, cfg.steer)
		}
		return m, m.submit(line)
	}

	// The ordinary typing path: every edit re-arms the autocomplete
	// pause (predict.go), a nil command without an llm-small row.
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette()
	m.layoutComposer()
	return m, tea.Batch(cmd, m.schedulePredict(cfg))
}

// steerLine hands one line to the turn in flight (pi's model): it
// lands at the loop's next block boundary and the model is asked
// again with it in context. The block shows as pending until the
// loop's "steer" event says it landed. When the loop reports no turn
// running (it ended before the key reached it), the line is sent as
// ordinary input instead, never dropped.
func (m *model) steerLine(line string, steer func(string) bool) tea.Cmd {
	if !steer(line) {
		return m.submit(line)
	}
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "user", text: line, steer: true, pending: true})
	m.nextID++
	m.refresh()
	m.vp.GotoBottom()
	return nil
}

// landSteer marks the oldest pending steer block with this text as
// landed; without one (the steer was typed in another view of this
// session — tui and web share the loop) it appends the block.
func (m *model) landSteer(id int, text string) {
	for i := range m.blocks {
		if b := &m.blocks[i]; b.pending && b.text == text {
			b.pending = false
			return
		}
	}
	m.blocks = append(m.blocks, block{id: id, kind: "user", text: text, steer: true})
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
	m.lastEnd = ""
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
				m.overlay.SetContent(m.fit(m.subTranscript(b, cfg)))
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
	m.overlay.SetContent(m.fit(sb.String()))
	m.overlay.SetYOffset(off)
}

func (m model) View() tea.View {
	v := tea.NewView(safeView(m.frame))
	v.WindowTitle = m.tabTitle() // the renderer sends it only when it changes
	v.AltScreen = true
	v.MouseMode = tea.MouseModeCellMotion // clicks toggle blocks; wheel scrolls
	// Key release events make hold-to-talk end on the release rather
	// than on the repeats stopping; asked for only while voice is on.
	v.KeyboardEnhancements = tea.KeyboardEnhancements{ReportEventTypes: m.v.mode != ""}
	if m.board.on {
		v.MouseMode = tea.MouseModeAllMotion // the board shows detail on hover
	}
	return v
}

// frame renders the full-screen content; View wraps it in a panic guard.
func (m model) frame() string {
	cfg := m.cfg.Load()
	// The strips above and below the composer come and go with state
	// the layout is not notified of (inspect mode, the pickers, a todo
	// event, a job starting). Re-fitting here — on the render's own
	// copy of the model — keeps the frame exactly as tall as the
	// terminal without every one of those paths remembering to say so.
	(&m).layoutComposer()
	if m.picking {
		return m.pickerView(cfg)
	}
	if m.mp.open {
		return m.modelPickerView(cfg)
	}
	if m.rw.open {
		return m.rewindView(cfg)
	}
	body := m.vp.View()
	if m.inspecting {
		body = m.overlay.View()
	} else if lines := m.overlayRows(); len(lines) > 0 {
		// The "/" palette: an overlay over the transcript's bottom
		// rows, directly above the composer — sized to its content,
		// never reflowing the layout under a filtering list.
		body = overlayBottom(body, lines)
	}
	out := body
	if rows := m.boardRows(cfg); len(rows) > 0 {
		if hover := m.hoverRows(cfg); len(hover) > 0 && !m.inspecting {
			body = overlayTop(body, hover)
		}
		out = strings.Join(rows, "\n") + "\n" + body
	}
	// The todo list sits directly above the composer, always, while
	// there is one: a plan you have to press a key to see is a plan
	// you forget the agent is working from. Its rows come out of the
	// transcript, like the status bar's and the job strip's.
	if rows := m.todoRows(cfg); len(rows) > 0 {
		out += "\n" + strings.Join(rows, "\n")
	}
	bar := m.statusBar(cfg)
	if m.srch.open {
		bar = m.searchLine(cfg)
	}
	out += "\n" + bar + "\n" + m.input.View()
	if strip := m.jobStrip(cfg); strip != "" {
		out += "\n" + strip
	}
	return out
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
// todoRows is the strip above the composer: the todo panel while
// there is a list and the user has not dismissed it. Empty otherwise,
// and empty in the overlays that own the bottom rows themselves.
func (m *model) todoRows(cfg *uiCfg) []string {
	if m.todoHidden || m.todoText == "" || m.inspecting || m.picking || m.mp.open {
		return nil
	}
	if len(m.overlayRows()) > 0 {
		return nil // the palette is already sitting there
	}
	return fit(m.todoPanel(cfg), m.stripRoom(cfg))
}

// stripRoom is how many rows the strips above the composer may take:
// what is left after the status bar, the composer, the job strip and
// one row of transcript. Zero on a pane too short to spare any.
func (m *model) stripRoom(cfg *uiCfg) int {
	used := 1 + min(max(m.input.Height(), 1), composerMaxLines) + len(m.jobRows(cfg)) + 1
	return max(m.height-used, 0)
}

// fit cuts rows to n, marking the cut when anything was dropped.
func fit(rows []string, n int) []string {
	if n <= 0 {
		return nil
	}
	if len(rows) <= n {
		return rows
	}
	if n == 1 {
		return rows[:1]
	}
	return append(rows[:n-1:n-1], "  …")
}

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
		lines = append(lines, xansi.Truncate(sanitizeText(l), max(m.width, 1), "…"))
	}
	return lines
}

// bareURL is a URL as it appears in rendered text: up to whitespace or
// an escape, not ending in sentence punctuation.
var bareURL = regexp.MustCompile(`https?://[^\s\x1b]*[^\s\x1b.,;:)'"]`)

// linkURLs makes each URL in rendered text an OSC 8 link, so a page
// URL in command output (/artifacts) is one click away.
func linkURLs(s string) string {
	return bareURL.ReplaceAllString(s, "\x1b]8;;$0\x07$0\x1b]8;;\x07")
}
