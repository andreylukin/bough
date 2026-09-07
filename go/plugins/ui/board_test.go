package ui

import (
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
	ansi "github.com/charmbracelet/x/ansi"

	"github.com/andreylukin/bough/plugins/attention"
	"github.com/andreylukin/bough/plugins/commands"
)

type fakeBoard struct {
	b      attention.Board
	sticky bool
}

func (f fakeBoard) Board() attention.Board { return f.b }
func (f fakeBoard) Sticky() bool           { return f.sticky }
func (f fakeBoard) Detail(kind, key string) []attention.Line {
	return []attention.Line{{Label: "asks", Text: "2 unresolved review threads (you)"}, {Label: "who", Text: "devin-ai-integration commented Sep 1"}, {Label: "for", Text: "NME-1664 Add nas-event-log to prod [code_review]"}}
}

func boardModel(t *testing.T, width int, src boardSource) model {
	t.Helper()
	var cfg atomic.Pointer[uiCfg]
	c := newCfg(defaultTheme(), defaultKeymap(), "bough", nil)
	c.board = src
	cfg.Store(c)
	return newModel(width, 30, func(string) {}, nil, &cfg)
}

func sample() attention.Board {
	now := time.Now()
	return attention.Board{
		Collected: now.Add(-2 * time.Hour),
		Me: []attention.Item{
			{Key: "bough · deps", Kind: "pr", Title: "bough · dependency updates", Status: "ci failing ×12", Detail: "review required", Since: now.Add(-3 * 24 * time.Hour), Count: 12},
			{Key: "orb#142", Kind: "pr", Title: "alert backtesting", Status: "open", Detail: "review required", Since: now.Add(-3 * time.Hour), URL: "https://github.com/x/orb#142", Summary: "review required, branch feat/x"},
		},
		Motion: []attention.Item{
			{Key: "bough#66", Kind: "pr", Title: "fix thing", Status: "ci failing", Detail: "session 8f3a · 1 review thread", Since: now.Add(-14 * time.Minute), Session: "8f3a1234-abcd", URL: "https://github.com/x/bough#66"},
		},
		Others: []attention.Item{
			{Key: "nas#46", Kind: "pr", Title: "fix sharding", Status: "ci green", Detail: "awaits Bradley", Since: now.Add(-2 * 24 * time.Hour)},
		},
	}
}

func TestBoardRowsWide(t *testing.T) {
	m := boardModel(t, 150, fakeBoard{b: sample(), sticky: true})
	if !m.board.on {
		t.Fatal("sticky: the board starts on")
	}
	m.takeBoard(sample())
	rows := m.boardRows(m.cfg.Load())
	plain := ansi.Strip(strings.Join(rows, "\n"))
	for _, want := range []string{
		"current work", "collected", "NEEDS ME 2", "IN MOTION 1", "WAITING ON OTHERS 1",
		"bough · deps ×12 ✕", "3d · review required", "alert backtesting", "orb#142 · 3h · review required",
		"fix thing ✕", "bough#66 · 14m · session 8f3a",
		"fix sharding ✓", "nas#46 · 2d · awaits Bradley",
	} {
		if !strings.Contains(plain, want) {
			t.Errorf("board lacks %q:\n%s", want, plain)
		}
	}
	// The stack's failing count equals its size, so it is not repeated.
	if strings.Contains(plain, "✕ ×12") {
		t.Errorf("stack repeats its count:\n%s", plain)
	}
	// A row with a session shows the spinner, not an age bar.
	if strings.Contains(plain, "▮ fix thing") || strings.Contains(plain, "▯ fix thing") {
		t.Errorf("in-motion row carries a bar:\n%s", plain)
	}
	// Every row fits the width; the frame gives the board its rows.
	for _, r := range rows {
		if w := ansi.StringWidth(r); w > 150 {
			t.Errorf("row over width (%d): %q", w, ansi.Strip(r))
		}
	}
	if !strings.Contains(ansi.Strip(m.frame()), "NEEDS ME") {
		t.Error("frame does not show the board")
	}
	if !m.boardMotion() {
		t.Error("a session on a row keeps the spinner ticking")
	}
}

func TestBoardNarrowAndToggle(t *testing.T) {
	m := boardModel(t, 80, fakeBoard{b: sample()})
	if m.board.on {
		t.Fatal("not sticky: the board starts off")
	}
	m.perform(commands.ActionBoard)
	m.takeBoard(sample())
	plain := ansi.Strip(strings.Join(m.boardRows(m.cfg.Load()), "\n"))
	if !strings.Contains(plain, "NEEDS ME 2") || strings.Contains(plain, "IN MOTION 1") || !strings.Contains(plain, "in motion 1 · waiting on others 1") {
		t.Errorf("narrow: one column plus counts:\n%s", plain)
	}
	m.perform(commands.ActionBoard)
	if m.board.on || len(m.boardRows(m.cfg.Load())) != 0 {
		t.Error("/current-work again hides the board")
	}
}

func TestBoardMarksChanges(t *testing.T) {
	m := boardModel(t, 150, fakeBoard{b: sample(), sticky: true})
	b := sample()
	m.takeBoard(b)
	if len(m.board.changed) != 0 {
		t.Fatal("the first read changes nothing")
	}
	b.Me[1].Status = "ci failing"
	m.takeBoard(b)
	if !m.board.changed["orb#142"] || m.board.changed["nas#46"] {
		t.Fatalf("changed: %v", m.board.changed)
	}
}

// The spinner chain has stopped by the time the first read arrives
// (nothing was running); a read with an in-motion row restarts it.
func TestBoardReadWakesSpinner(t *testing.T) {
	m := boardModel(t, 150, fakeBoard{b: sample(), sticky: true})
	mm, cmd := m.Update(boardMsg{sample()})
	m = mm.(model)
	if cmd == nil {
		t.Fatal("a read with motion returns commands")
	}
	// Run the batch: one of its messages must be the spinner's tick.
	msgs := drain(cmd)
	found := false
	for _, x := range msgs {
		if _, ok := x.(spinner.TickMsg); ok {
			found = true
		}
	}
	if !found {
		t.Fatalf("no spinner tick among %T", msgs)
	}
}

// drain runs a command (batches included) and collects its messages.
func drain(cmd tea.Cmd) []tea.Msg {
	if cmd == nil {
		return nil
	}
	var out []tea.Msg
	switch msg := cmd().(type) {
	case tea.BatchMsg:
		for _, c := range msg {
			out = append(out, drain(c)...)
		}
	default:
		out = append(out, msg)
	}
	return out
}

func TestBoardHoverClickAndOffset(t *testing.T) {
	m := boardModel(t, 150, fakeBoard{b: sample(), sticky: true})
	m.takeBoard(sample())
	cfg := m.cfg.Load()
	h := m.boardHeight(cfg)
	if h < 4 {
		t.Fatalf("board height %d", h)
	}
	// Row 2 is the first item of each column; column width is 49.
	if it, ok := m.boardItemAt(cfg, 5, 2); !ok || it.Key != "bough · deps" {
		t.Fatalf("item at (5,2): %+v %v", it, ok)
	}
	if it, ok := m.boardItemAt(cfg, 60, 3); !ok || it.Key != "bough#66" {
		t.Fatalf("item at (60,3): %+v %v", it, ok)
	}
	if it, ok := m.boardItemAt(cfg, 5, 4); !ok || it.Key != "orb#142" {
		t.Fatalf("item at (5,4): %+v %v", it, ok)
	}
	if _, ok := m.boardItemAt(cfg, 60, 4); ok {
		t.Fatal("no second item in motion")
	}
	// Motion over a row hovers it: the name is underlined and a detail
	// box with the link covers the transcript's top rows.
	mm, fetch := m.Update(tea.MouseMotionMsg{X: 5, Y: 4})
	m = mm.(model)
	if m.board.hover != "orb#142" {
		t.Fatalf("hover = %q", m.board.hover)
	}
	if fetch == nil {
		t.Fatal("hovering a row must fetch its detail")
	}
	frame := m.frame()
	if !strings.Contains(ansi.Strip(frame), "▸ alert backtesting") || !strings.Contains(frame, "https://github.com/x/orb#142") || !strings.Contains(ansi.Strip(frame), "reading the graph") {
		t.Errorf("hover box missing:\n%s", ansi.Strip(frame))
	}
	// The detail arrives and replaces the placeholder.
	mm, _ = m.Update(hoverMsg{key: "orb#142", lines: fakeBoard{}.Detail("pr", "orb#142")})
	m = mm.(model)
	plainFrame := ansi.Strip(m.frame())
	for _, want := range []string{"asks     2 unresolved review threads (you)", "who      devin-ai-integration commented Sep 1", "for      NME-1664 Add nas-event-log to prod [code_review]"} {
		if !strings.Contains(plainFrame, want) {
			t.Errorf("detail missing %q:\n%s", want, plainFrame)
		}
	}
	if strings.Contains(plainFrame, "reading the graph") {
		t.Error("placeholder outlived the detail")
	}
	// The name row carries a real terminal link.
	if !strings.Contains(frame, "\x1b]8;;https://github.com/x/orb#142") {
		t.Error("no OSC 8 link on the row")
	}
	// Motion below the board clears the hover.
	mm, _ = m.Update(tea.MouseMotionMsg{X: 5, Y: h + 3})
	m = mm.(model)
	if m.board.hover != "" {
		t.Fatal("hover survives leaving the board")
	}
	// A click on a row returns the open command; on the deps stack (no URL) nothing.
	if _, cmd := m.Update(tea.MouseClickMsg{X: 5, Y: 4, Button: tea.MouseLeft}); cmd == nil {
		t.Error("click on a linked row does nothing")
	}
	if _, cmd := m.Update(tea.MouseClickMsg{X: 5, Y: 2, Button: tea.MouseLeft}); cmd != nil {
		t.Error("click on a stack opens something")
	}
	// A click on the status bar still opens the picker: its row is
	// the viewport's height below the board, not from the top.
	if got := shiftMouse(tea.Mouse{Y: h + m.vp.Height()}, h).Y; got != m.vp.Height() {
		t.Errorf("shifted y = %d, want %d", got, m.vp.Height())
	}
	if m.View().MouseMode != tea.MouseModeAllMotion {
		t.Error("the board needs motion events")
	}
}
