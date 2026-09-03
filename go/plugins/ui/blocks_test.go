package ui

// Semantic block rendering: role markers, boxes, collapse, markdown,
// unknown kinds.

import (
	"strconv"
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
)

func TestRenderUserMarker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "hello there")
	if p := d.plain(); !strings.Contains(p, "❯ hello there") {
		t.Errorf("user block missing ❯ marker:\n%s", p)
	}
}

func TestRenderAssistantMarker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "plain reply")
	p := d.plain()
	if !strings.Contains(p, "●") || !strings.Contains(p, "bough") {
		t.Errorf("assistant block missing ● bough header:\n%s", p)
	}
	if !strings.Contains(p, "plain reply") {
		t.Errorf("assistant text missing:\n%s", p)
	}
}

func TestRenderCodeHeaderAndBox(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("code", `tools.bash("echo hi")`)
	d.press(keyTab()) // focus the code block (starts collapsed)
	d.press(keyEnter())
	p := d.plain()
	if !strings.Contains(p, "▾ Ran: echo hi (1 line)") || strings.Contains(p, `(1 line): tools.bash`) {
		t.Errorf("code block missing expanded disclosure header:\n%s", p)
	}
	if !strings.Contains(p, `tools.bash("echo hi")`) {
		t.Errorf("code text missing:\n%s", p)
	}
	if !strings.Contains(p, "╰") || !strings.Contains(p, "╮") {
		t.Errorf("code block missing rounded border:\n%s", p)
	}
}

func TestRenderResultHeaderAndBox(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", "hi from codemode")
	d.press(keyTab()) // focus the result block (starts collapsed)
	d.press(keyEnter())
	p := d.plain()
	if !strings.Contains(p, "▾ result (1 line): hi from codemode") {
		t.Errorf("result block missing expanded disclosure header:\n%s", p)
	}
	if !strings.Contains(p, "│ hi from codemode") {
		t.Errorf("result text missing from box:\n%s", p)
	}
}

func TestRenderErrorMarker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("error", "provider exploded")
	if p := d.plain(); !strings.Contains(p, "✗ provider exploded") {
		t.Errorf("error block missing ✗ marker:\n%s", p)
	}
}

func TestRenderDoneDivider(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("done", "")
	if p := d.plain(); !strings.Contains(p, strings.Repeat("─", 40)) {
		t.Errorf("done divider missing (want 40 ─ at width 80):\n%s", p)
	}
}

func TestRenderDoneDividerNarrow(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 20, 24, cfgWith(t, nil, nil, nil))
	d.event("done", "")
	p := d.plain()
	if !strings.Contains(p, strings.Repeat("─", 18)) {
		t.Errorf("narrow divider missing (want 18 ─ at width 20):\n%s", p)
	}
	if strings.Contains(p, strings.Repeat("─", 19)) {
		t.Errorf("narrow divider too wide:\n%s", p)
	}
}

func TestRenderUnknownKindIgnoredGracefully(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("telemetry", "cpu 3%") // future kind: rendered dim, no crash
	p := d.plain()
	if !strings.Contains(p, "telemetry") || !strings.Contains(p, "cpu 3%") {
		t.Errorf("unknown kind should render kind+text:\n%s", p)
	}
}

func TestTranscriptOrder(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "q1")
	d.event("assistant", "a1")
	d.event("done", "")
	p := d.plain()
	iq, ia := strings.Index(p, "q1"), strings.Index(p, "a1")
	id := strings.Index(p, "────")
	if iq < 0 || ia < 0 || id < 0 || !(iq < ia && ia < id) {
		t.Errorf("blocks out of order (q1@%d a1@%d done@%d):\n%s", iq, ia, id, p)
	}
}

// --- collapse ---

func TestLongResultStartsCollapsed(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	if !d.m.blocks[0].collapsed {
		t.Fatal("20-line result should start collapsed")
	}
	p := d.plain()
	if !strings.Contains(p, "▸ result (20 lines): l0xxx") {
		t.Errorf("collapsed result missing header with line count and preview:\n%s", p)
	}
	if strings.Contains(p, "l9xxx") {
		t.Errorf("collapsed result body should be hidden:\n%s", p)
	}
}

func TestBoundaryResultDoesNotCollapse(t *testing.T) {
	t.Parallel()
	// The size threshold only exists under collapse: "large" (the
	// default "all" collapses everything).
	cfg := cfgWith(t, nil, nil, nil)
	cfg.collapse = "large"
	d := newDrv(t, 80, 24, cfg)
	d.event("result", nLines(collapseAt)) // exactly at the threshold
	if d.m.blocks[0].collapsed {
		t.Errorf("%d-line result should not collapse (threshold is >%d)", collapseAt, collapseAt)
	}
	d.event("result", nLines(collapseAt+1))
	if !d.m.blocks[1].collapsed {
		t.Errorf("%d-line result should collapse", collapseAt+1)
	}
}

func TestFocusEnterExpands(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.press(keyTab()) // block_next: focus the result
	d.press(keyEnter())
	if d.m.blocks[0].collapsed {
		t.Fatal("enter on the focused result should expand it")
	}
	d.press(keyPgUp()) // the 20-line body pushed the header off the top
	if p := d.plain(); !strings.Contains(p, "▾ result (20 lines)") {
		t.Errorf("expanded result missing ▾ header:\n%s", p)
	}
}

func TestFocusEnterTogglesBack(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.press(keyTab())
	d.press(keyEnter())
	d.press(keyEnter())
	if !d.m.blocks[0].collapsed {
		t.Error("second enter should re-collapse")
	}
	if p := d.plain(); !strings.Contains(p, "▸ result (20 lines)") {
		t.Errorf("re-collapsed result missing ▸ header:\n%s", p)
	}
}

func TestRemappedToggleHitsNewestBlock(t *testing.T) {
	t.Parallel()
	// collapse_toggle remapped off enter: with nothing focused it
	// toggles the newest collapsible block.
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"collapse_toggle": "ctrl+g"}, nil))
	d.event("result", nLines(20))
	d.event("result", nLines(30))
	d.press(keyCtrl('g'))
	if !d.m.blocks[0].collapsed {
		t.Error("first result should stay collapsed")
	}
	if d.m.blocks[1].collapsed {
		t.Error("newest result should have been expanded")
	}
}

func TestToggleNoResultsIsNoop(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "no results here")
	d.press(keyTab())   // no collapsible block to focus
	d.press(keyEnter()) // must not panic or alter blocks
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "assistant" {
		t.Error("toggle with no results changed the transcript")
	}
}

// --- markdown ---

func TestMarkdownHeadingRenders(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "# Big Title\n\nbody text")
	p := d.plain()
	if !strings.Contains(p, "Big Title") || !strings.Contains(p, "body text") {
		t.Errorf("markdown heading/body not visible:\n%s", p)
	}
}

func TestMarkdownBoldRenders(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "some **very bold** words")
	if p := d.plain(); !strings.Contains(p, "very bold") {
		t.Errorf("bold text not visible (raw or styled):\n%s", p)
	}
}

func TestMarkdownCacheClearedOnResize(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "cached line")
	if len(d.m.mdCache) == 0 {
		t.Fatal("assistant render should populate the markdown cache")
	}
	d.feed(windowSize(120, 40))
	if len(d.m.mdCache) != 1 { // refresh re-rendered the one block at the new width
		t.Errorf("resize should rebuild the cache, got %d entries", len(d.m.mdCache))
	}
}

func TestRenderSystemBlock(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("system", "cleared\nsecond line")
	b := &d.m.blocks[0]
	if !b.collapsible() || !b.collapsed {
		t.Fatalf("a multi-line system note is detail: closed and collapsible, got %+v", b)
	}
	b.collapsed = false
	d.m.refresh()
	if p := d.plain(); !strings.Contains(p, "cleared") || !strings.Contains(p, "second line") {
		t.Errorf("system block text missing:\n%s", p)
	}
	if p := d.plain(); strings.Contains(p, "●") {
		t.Errorf("an open system block has no speaker header:\n%s", p)
	}
}

// --- transcript readability (8-persona audit) ---

func TestEmissionOrderProseAroundCode(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "Looking…\n```js\nconsole.log(1)\n```\nDone.")
	d.event("code", "console.log(1)")
	d.event("result", "1")
	d.event("done", "")
	p := d.plain()
	iL, iC, iR, iD := strings.Index(p, "Looking…"), strings.Index(p, "▸ code js (1 line)"),
		strings.Index(p, "▸ result (1 line): 1"), strings.Index(p, "Done.")
	if iL < 0 || iC < 0 || iR < 0 || iD < 0 || !(iL < iC && iC < iR && iR < iD) {
		t.Errorf("transcript not in emission order (prose@%d code@%d result@%d prose@%d):\n%s", iL, iC, iR, iD, p)
	}
}

func TestThinkingSpanCollapses(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "<thinking_analyses>alpha\nbeta\ngamma</thinking_analyses>\n<system_warning>budget low</system_warning>\nHello there")
	p := d.plain()
	if !strings.Contains(p, "▸ thinking (3 lines)") {
		t.Errorf("thinking span should be a collapsed row:\n%s", p)
	}
	if strings.Contains(p, "beta") || strings.Contains(p, "<thinking") {
		t.Errorf("thinking body must be hidden:\n%s", p)
	}
	if strings.Contains(p, "budget low") {
		t.Errorf("system_warning span must be dropped:\n%s", p)
	}
	if !strings.Contains(p, "Hello there") {
		t.Errorf("prose after the spans should render:\n%s", p)
	}
}

func TestCodeLabels(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("x", 70)
	for code, want := range map[string]string{
		`tools.patch("main.go", a, b)`:   "Edited main.go",
		`const s = tools.view('go.mod')`: "Read go.mod",
		"tools.bash(`go test ./...`)":    "Ran: go test ./...",
		`tools.bash("` + long + `")`:     "Ran: " + long[:60] + "…",
		`tools.ask("ok?", ["y","n"])`:    "Asked you",
		`tools.spawn("review the diff")`: "Subagent: review the diff",
		`console.log(1)`:                 "code js",
		`tools.bash(cmd)`:                "Ran: ",
		"tools.patch(\"calc.py\", a, b); console.log(tools.bash(\"python3 test_calc.py\"))": "Edited calc.py · Ran: python3 test_calc.py",
		"tools.bash(\"ls\"); tools.bash(\"ls\"); tools.bash(\"pwd\")":                       "Ran: ls · Ran: pwd",
	} {
		if got := codeLabel(code); got != want {
			t.Errorf("codeLabel(%q) = %q, want %q", code, got, want)
		}
	}
}

func TestCollapseExpandFeedbackAndPreviewCap(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, uiActionReg(t, map[string]commands.UIAction{
		"collapse": commands.ActionCollapse, "expand": commands.ActionExpand,
	}))
	d.event("result", nLines(10))
	d.event("result", nLines(previewCap+1))
	d.typeStr("/expand")
	d.press(keyEnter())
	p := d.plain()
	if !strings.Contains(p, "expanded 1 block (blocks over 200 lines stay collapsed unless focused)") {
		t.Errorf("/expand should report what it did:\n%s", p)
	}
	if d.m.blocks[0].collapsed || !d.m.blocks[1].collapsed {
		t.Error("/expand should open the short block and leave the huge one collapsed")
	}
	d.typeStr("/collapse")
	d.press(keyEnter())
	// Two: the short result and the /expand command's own output — a
	// note is collapsible too, so collapse-all takes it.
	if !strings.Contains(d.plain(), "collapsed 2 blocks") {
		t.Errorf("/collapse should report what it did:\n%s", d.plain())
	}
	// Focused, the huge block expands on request.
	d.m.focusID = d.m.blocks[1].id
	d.typeStr("/expand")
	d.press(keyEnter())
	if d.m.blocks[1].collapsed {
		t.Error("/expand should open a focused huge block")
	}
}

func TestQueuedPromptMarked(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("first")
	d.press(keyEnter())
	d.typeStr("second")
	d.press(keyEnter())
	if !d.m.blocks[1].queued || !strings.Contains(d.plain(), "❯ second (queued)") {
		t.Errorf("a prompt submitted mid-turn should read (queued):\n%s", d.plain())
	}
	d.event("assistant", "one")
	d.event("done", "")
	if d.m.blocks[1].queued || strings.Contains(d.plain(), "(queued)") {
		t.Errorf("the queued mark should clear once its turn starts:\n%s", d.plain())
	}
	if !d.m.running {
		t.Error("the queued turn is now in flight: spinner must keep running")
	}
}

func TestLongUserPromptWraps(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", strings.Repeat("word ", 30)+"TAIL")
	if !strings.Contains(d.plain(), "TAIL") {
		t.Errorf("long prompt should wrap, not clip at the right edge:\n%s", d.plain())
	}
}

func TestTurnWithoutReplyMarked(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "hi")
	d.event("assistant", "```js\nbroken(\n") // malformed fence: nothing visible
	d.event("done", "")
	if !strings.Contains(d.plain(), "✗ turn ended without a reply") {
		t.Errorf("empty turn needs an explicit end marker:\n%s", d.plain())
	}
	d.event("user", "again")
	d.event("assistant", "a reply")
	d.event("done", "")
	if strings.Count(d.plain(), "turn ended without a reply") != 1 {
		t.Errorf("a turn with a reply must not be marked:\n%s", d.plain())
	}
}

func TestDoneSummaryRendersFilesAndExit(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "wrote it")
	d.feed(eventMsg{Kind: "done", Data: map[string]any{"files": []any{"main.go", "main_test.go"}, "exit": float64(0)}})
	if !strings.Contains(d.plain(), "✔ wrote main.go, main_test.go · exit 0") {
		t.Errorf("done entry data should render:\n%s", d.plain())
	}
	d.event("assistant", "plain")
	d.event("done", "") // no data: divider only
	if strings.Count(d.plain(), "✔") != 1 {
		t.Errorf("a done without data must render no summary:\n%s", d.plain())
	}
}

func TestSafeViewRecovers(t *testing.T) {
	t.Parallel()
	out := safeView(func() string { panic("slice bounds out of range [:5] with length 2") })
	if out != "✗ render failed: slice bounds out of range [:5] with length 2" {
		t.Errorf("panic should become one error line, got %q", out)
	}
	if strings.Contains(out, "goroutine") {
		t.Error("no stack trace in the transcript")
	}
}

func TestNarrowFrameNoPanic(t *testing.T) {
	t.Parallel()
	for _, w := range []int{0, 1, 3, 8} {
		d := newDrv(t, w, 2, cfgWith(t, nil, nil, nil))
		for _, k := range []string{"user", "assistant", "code", "result", "error", "system", "todo", "done"} {
			d.event(k, "some text\nmore")
		}
		if p := d.plain(); strings.Contains(p, "render failed") {
			t.Errorf("width %d: frame render panicked:\n%s", w, p)
		}
	}
}

func TestBinaryResultPlaceholder(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	bin := "\xcf\xfa\xed\xfe\x07\x00\x00\x01\x03\x00\x00\x80\x02\x00\x00\x00\x19\x00\x00\x00"
	d.event("result", bin)
	if !strings.Contains(d.plain(), "▸ result (1 line): (binary, 20 bytes)") {
		t.Errorf("binary output should render as a placeholder:\n%s", d.plain())
	}
	d.event("result", "plain text\nwith a tab\there")
	if d.m.blocks[1].text != "plain text\nwith a tab\there" {
		t.Error("text output must pass through untouched")
	}
}

func TestErrorNativeSuffixStripped(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("error", "readFile: open x: no such file or directory at github.com/andreylukin/bough/plugins/codemode.(*rt).readFile-fm (native)")
	p := d.plain()
	if strings.Contains(p, "(native)") || strings.Contains(p, "github.com/andreylukin") {
		t.Errorf("goja stack suffix should be stripped:\n%s", p)
	}
	if !strings.Contains(p, "✗ readFile: open x: no such file or directory") {
		t.Errorf("error text lost:\n%s", p)
	}
}

func TestReplaySystemEntry(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/x.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "system", Data: map[string]any{"text": "unknown command: /x (try /help)"}},
	}}
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	if len(d.m.blocks) != 2 || d.m.blocks[0].kind != "system" { // + the resumed row
		t.Fatalf("system entry should replay as a system block, got %+v", d.m.blocks)
	}
	if p := d.plain(); !strings.Contains(p, "unknown command: /x") {
		t.Errorf("replayed system text missing:\n%s", p)
	}
}

// Streamed fragments grow one live assistant block (with a cursor);
// the final assistant event replaces it with the rendered reply, so
// nothing is shown twice.
func TestAssistantDeltasBuildLiveBlockThenReplaced(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("hi")
	d.press(keyEnter())
	d.event("assistant-delta", "Hel")
	d.event("assistant-delta", "lo **there**")
	n := len(d.m.blocks)
	if b := d.m.blocks[n-1]; !b.live || b.text != "Hello **there**" {
		t.Fatalf("live block = %+v", b)
	}
	if p := d.plain(); !strings.Contains(p, "Hello **there**▌") {
		t.Fatalf("streaming text should render raw with a cursor:\n%s", p)
	}
	d.event("assistant", "Hello **there**")
	d.event("done", "")
	if len(d.m.blocks) != n+1 { // live block replaced, done added
		t.Fatalf("blocks = %d, want %d", len(d.m.blocks), n+1)
	}
	p := d.plain()
	if strings.Contains(p, "▌") || strings.Count(p, "Hello") != 1 || strings.Contains(p, "**") {
		t.Fatalf("final reply should replace the live block and render markdown:\n%s", p)
	}
}

// A subagent's events fold into ONE card per worker, open by default
// under every policy: the header carries the state, call count and
// elapsed; the body the whole task, then the last call and its first
// output line while running, then the child's report (capped) once
// done. Collapsed, it is one line with the task cut short.
func TestSubagentEventsFoldIntoOneCard(t *testing.T) {
	for _, mode := range []string{"all", "large"} {
		m := testModelCollapse(t, mode)
		w := map[string]any{"worker": 2}
		m.addEvent(Event{Kind: "sub:start", Text: "count the files", Data: w})
		m.addEvent(Event{Kind: "sub:assistant", Text: "on it\n```js\ntools.bash(\"ls\")\n```", Data: w})
		m.addEvent(Event{Kind: "sub:code", Text: "console.log(tools.bash(\"ls\"))", Data: w})
		if len(m.blocks) != 1 || m.blocks[0].kind != "spawn" || m.blocks[0].collapsed || !m.blocks[0].collapsible() {
			t.Fatalf("%s: want one open spawn card, got %+v", mode, m.blocks)
		}
		card := stripANSI(m.render(&m.blocks[0], m.cfg.Load()))
		for _, want := range []string{"subagent 2", "running", "1 call", "┃ count the files", "Ran: ls"} {
			if !strings.Contains(card, want) {
				t.Fatalf("%s: running card missing %q:\n%s", mode, want, card)
			}
		}
		m.addEvent(Event{Kind: "sub:result", Text: "a\nb\n", Data: w})
		if card = stripANSI(m.render(&m.blocks[0], m.cfg.Load())); !strings.Contains(card, "Ran: ls\n") || !strings.Contains(card, "┃   a") {
			t.Fatalf("%s: running card should show the last output line under the call:\n%s", mode, card)
		}
		m.addEvent(Event{Kind: "sub:assistant", Text: "Status: ok\nFindings: two files", Data: w})
		m.addEvent(Event{Kind: "sub:done", Data: map[string]any{"worker": 2, "status": "ok", "steps": 2}})
		if len(m.blocks) != 1 {
			t.Fatalf("%s: done must not add blocks, got %d", mode, len(m.blocks))
		}
		card = stripANSI(m.render(&m.blocks[0], m.cfg.Load()))
		if strings.Contains(card, "<1s") {
			t.Fatalf("%s: an instant (replayed) card must not claim an elapsed time:\n%s", mode, card)
		}
		if !strings.Contains(card, "✔") || !strings.Contains(card, "done · 1 call") || strings.Contains(card, "Ran: ls") ||
			!strings.Contains(card, "Findings: two files") || strings.Contains(card, "on it") {
			t.Fatalf("%s: finished card shows ✔, the count, the report, no call or earlier replies:\n%s", mode, card)
		}
		m.blocks[0].collapsed = true
		head := stripANSI(m.render(&m.blocks[0], m.cfg.Load()))
		if strings.Count(head, "\n") != 0 || !strings.Contains(head, "subagent 2 · count the files · done") {
			t.Fatalf("%s: collapsed card should be one line with the task: %q", mode, head)
		}
		if n := len(m.blocks[0].sub.log); n != 4 {
			t.Fatalf("%s: child transcript has %d events, want 4", mode, n)
		}
	}
}

// A running card's spinner moves on spinner ticks: the pane is rebuilt
// on the tick, not only on the next event.
func TestRunningSpawnCardSpinsOnTick(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.feed(keyEnter()) // submit without running its commands: the turn stays in flight
	d.feed(eventMsg{Kind: "sub:start", Text: "task", Data: map[string]any{"worker": 1}})
	before := d.plain()
	for range 3 {
		d.feed(d.m.spin.Tick())
	}
	if d.plain() == before {
		t.Fatalf("running card should redraw on ticks (running=%v):\nBEFORE\n%s\nAFTER\n%s", d.m.running, before, d.plain())
	}
}

func TestReportBodyStripsScaffolding(t *testing.T) {
	t.Parallel()
	got := reportBody("REPORT\n\nStatus: ok\n\nFindings:\n- two files\n\n\nOpen: none\n")
	if got != "Findings:\n- two files\n\nOpen: none" {
		t.Fatalf("reportBody = %q", got)
	}
}

// A long report is capped inline; the note points at the transcript.
func TestSubagentReportCapped(t *testing.T) {
	m := testModel(t)
	w := map[string]any{"worker": 1}
	m.addEvent(Event{Kind: "sub:start", Text: "t", Data: w})
	m.addEvent(Event{Kind: "sub:assistant", Text: nLines(reportCap + 5), Data: w})
	m.addEvent(Event{Kind: "sub:done", Data: map[string]any{"worker": 1, "status": "ok"}})
	card := stripANSI(m.render(&m.blocks[0], m.cfg.Load()))
	if !strings.Contains(card, "… +5 lines") || strings.Contains(card, "l"+strconv.Itoa(reportCap+1)) {
		t.Fatalf("report should be capped at %d lines:\n%s", reportCap, card)
	}
}

func TestSubagentCardFailureStates(t *testing.T) {
	m := testModel(t)
	w := map[string]any{"worker": 1}
	m.addEvent(Event{Kind: "sub:start", Text: "t", Data: w})
	m.addEvent(Event{Kind: "sub:error", Text: "error: boom\nstack", Data: w})
	m.addEvent(Event{Kind: "sub:done", Data: map[string]any{"worker": 1, "status": "error"}})
	if h := stripANSI(m.render(&m.blocks[0], m.cfg.Load())); !strings.Contains(h, "✗") || !strings.Contains(h, "error: boom") || strings.Contains(h, "stack") {
		t.Fatalf("error card: %q", h)
	}
	m.addEvent(Event{Kind: "sub:start", Text: "u", Data: map[string]any{"worker": 2}})
	m.addEvent(Event{Kind: "sub:assistant", Text: "Status: failed\nFindings: nope", Data: map[string]any{"worker": 2}})
	m.addEvent(Event{Kind: "sub:done", Data: map[string]any{"worker": 2, "status": "failed"}})
	if h := stripANSI(m.render(&m.blocks[1], m.cfg.Load())); !strings.Contains(h, "✗") || !strings.Contains(h, "reported failure") {
		t.Fatalf("failed card: %q", h)
	}
}

// Events for a worker without a start (old histories) still get a card.
func TestSubagentCardWithoutStart(t *testing.T) {
	m := testModel(t)
	m.addEvent(Event{Kind: "sub:result", Text: "x", Data: map[string]any{"worker": 3}})
	if len(m.blocks) != 1 || m.blocks[0].kind != "spawn" || m.blocks[0].sub.worker != 3 {
		t.Fatalf("blocks = %+v", m.blocks)
	}
}

// ctrl+o on a focused card opens the child's transcript in the overlay;
// esc (or ctrl+o) returns to the parent. Elsewhere ctrl+o is history.
func TestSubagentDiveOverlay(t *testing.T) {
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, histWith("/tmp/h.jsonl", "prompt")))
	w := map[string]any{"worker": 1}
	d.m.addEvent(Event{Kind: "sub:start", Text: "list files", Data: w})
	d.m.addEvent(Event{Kind: "sub:code", Text: "tools.bash(\"ls\")", Data: w})
	d.m.addEvent(Event{Kind: "sub:result", Text: "a.go\nb.go", Data: w})
	d.m.addEvent(Event{Kind: "sub:assistant", Text: "Status: ok\nFindings: 2 go files", Data: w})
	d.m.addEvent(Event{Kind: "sub:done", Data: map[string]any{"worker": 1, "status": "ok", "steps": 2}})
	d.feed(keyTab()) // focus the newest card
	d.feed(keyCtrl('o'))
	if !d.m.inspecting || d.m.diving == 0 {
		t.Fatalf("ctrl+o on a focused card should dive (inspecting=%v diving=%d)", d.m.inspecting, d.m.diving)
	}
	p := d.plain()
	for _, want := range []string{"subagent 1", "list files", "Ran: ls", "a.go", "Findings: 2 go files", "✔ done · 2 steps", "subagent transcript · esc to close"} {
		if !strings.Contains(p, want) {
			t.Fatalf("dive overlay missing %q:\n%s", want, p)
		}
	}
	d.feed(keyEsc())
	if d.m.inspecting || d.m.diving != 0 {
		t.Fatal("esc should close the dive")
	}
	// Focus off the card: ctrl+o is the history inspector again.
	d.m.focusID = -1
	d.feed(keyCtrl('o'))
	if !d.m.inspecting || d.m.diving != 0 || !strings.Contains(d.plain(), "history") {
		t.Fatalf("ctrl+o without a focused card should open history:\n%s", d.plain())
	}
}

// A cancelled turn already says "cancelled"; the "without a reply"
// marker is for a turn that silently produced nothing.
func TestCancelledTurnHasNoWithoutReplyMarker(t *testing.T) {
	m := testModel(t)
	m.addEvent(Event{Kind: "user", Text: "go"})
	m.addEvent(Event{Kind: "cancelled"})
	m.addEvent(Event{Kind: "done"})
	for _, b := range m.blocks {
		if strings.Contains(b.text, "without a reply") {
			t.Fatalf("redundant marker after cancel: %+v", m.blocks)
		}
	}
}

// A fence being streamed is not typed out as text: the prose before it
// shows, then a "writing code" note, and the code block arrives whole
// when the reply lands.
func TestStreamingHidesTheFenceBeingWritten(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.event("assistant-delta", "Let me check.\n``")
	if p := d.plain(); strings.Contains(p, "``") || !strings.Contains(p, "Let me check.") {
		t.Fatalf("a half fence must be held back:\n%s", p)
	}
	d.event("assistant-delta", "`js\nconsole.log(tools.bash(\"ls\"))")
	p := d.plain()
	if strings.Contains(p, "console.log") || strings.Contains(p, "```") {
		t.Fatalf("the code being written leaked into the live view:\n%s", p)
	}
	if !strings.Contains(p, "Let me check.") || !strings.Contains(p, "▸ writing code…") {
		t.Fatalf("want the prose and the note:\n%s", p)
	}
	prose, coding := liveView("```js\nx")
	if prose != "" || !coding {
		t.Fatalf("fence-first reply: %q %v", prose, coding)
	}
}

func TestDoneSummaryMarksFailureAndHidesKilled(t *testing.T) {
	t.Parallel()
	if got := doneSummary(nil, 1, true); got != "✗ exit 1" {
		t.Errorf("nonzero exit = %q", got)
	}
	if got := doneSummary(nil, -1, true); got != "" {
		t.Errorf("killed (-1) should render nothing, got %q", got)
	}
	if got := doneSummary([]string{"a"}, -1, true); got != "✔ wrote a" {
		t.Errorf("killed with files = %q", got)
	}
	if got := collapseNote(true, 0); !strings.Contains(got, "already folded") {
		t.Errorf("zero collapse note = %q", got)
	}
}
