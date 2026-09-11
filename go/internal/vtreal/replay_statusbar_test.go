package vtreal

// The status bar on a real terminal: what it shows at five widths,
// what it drops first when the pane is narrow, what a long session
// title does to the left side, the spinner while a turn streams, and
// a flash taking the right side over.
//
// The bar's parts come from the session, not from a network: the llm
// row is llm-openai pointed at a dead port (it names a model and a
// context window without ever being called) and the tally, the cost
// and the last request's size are read off a seeded history file by
// the cost row. Only the spinner test runs a turn, and that one
// replays a tape.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// statusbarSeed writes a history file of the given entries into a
// fresh temp dir and returns its path. The app appends to the file it
// resumes, so every test gets its own copy.
func statusbarSeed(t *testing.T, name string, entries ...map[string]any) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), name)
	var sb strings.Builder
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = time.Date(2026, 9, 10, 10, 0, i, 0, time.UTC).Format(time.RFC3339)
		b, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

func statusbarEntry(kind string, data map[string]any) map[string]any {
	return map[string]any{"kind": kind, "data": data}
}

// The usage a finished turn recorded: the cost row sums these into the
// bar's "↑in ↓out · $cost · N% ctx".
var statusbarUsage = map[string]any{"in": 12300, "out": 3400, "last_in": 357000, "cost": 0.052}

// statusbarCfg is an idle session: a model that is named but never
// called, the cost row over it, and a resumed history.
func statusbarCfg(hist string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: llm-openai
  config: {model: gpt-5.6-luna, base_url: "http://127.0.0.1:1"}
- id: cost
  plugin: cost
- id: codemode
  plugin: codemode
- id: commands
  plugin: commands
- id: history
  plugin: history
  config: {file: %q}
- id: loop
  plugin: loop
- id: ui
  plugin: ui
`, hist) + statusbarQuietRows
}

// Rows that would call the model on their own: they have nowhere to
// call here, and their replies would eat a replayed tape.
const statusbarQuietRows = `
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
`

// statusbarLine is the bar: the row directly above the composer. Found
// by position, not by the keys hint, because a long enough left side
// truncates the hint away (see TestStatusbarLongTitleTruncates).
func statusbarLine(a *app, s string) string {
	a.t.Helper()
	ls := strings.Split(s, "\n")
	r := composerRow(ls)
	if r < 1 {
		a.t.Fatalf("no composer, so no status bar above it:\n%s", s)
	}
	return ls[r-1]
}

var (
	statusbarElapsed = regexp.MustCompile(`\d+s · `)
	statusbarCtx     = regexp.MustCompile(`\d+% ctx`)
)

// The right side degrades in the documented order as the pane
// narrows: tokens go first, then the model, then the context, then the
// cost, and "? keys" is the floor. Checked as an order, not as five
// hardcoded strings: what matters is that no part outlives one that is
// documented to survive it, and that every part that fits at one width
// still fits at every wider one.
func TestStatusbarDegradesByWidth(t *testing.T) {
	t.Parallel()
	widths := []int{40, 60, 80, 100, 160} // widest last
	// The parts in the order they are dropped, last survivor last.
	parts := []struct {
		name string
		has  func(string) bool
	}{
		{"tokens", func(b string) bool { return strings.Contains(b, "↑12.3k ↓3.4k") }},
		{"model", func(b string) bool { return strings.Contains(b, "gpt-5.6-luna") }},
		{"ctx", func(b string) bool { return statusbarCtx.MatchString(b) }},
		{"cost", func(b string) bool { return strings.Contains(b, "$0.052") }},
		{"keys", func(b string) bool { return strings.Contains(b, "? keys") }},
	}
	seen := make([]map[string]bool, len(widths))
	bars := make([]string, len(widths))
	for i, w := range widths {
		hist := statusbarSeed(t, "usage.jsonl",
			statusbarEntry("meta", map[string]any{"cwd": "/tmp/demo"}),
			statusbarEntry("input", map[string]any{"text": "how much have we spent"}),
			statusbarEntry("assistant", map[string]any{"text": "```stop\nnot much\n```"}),
			map[string]any{"kind": "done", "data": map[string]any{"usage": statusbarUsage}},
		)
		a := startCfg(t, w, 24, statusbarCfg(hist))
		s := a.settled()
		a.check(fmt.Sprintf("idle at %d columns", w))
		bar := statusbarLine(a, s)
		bars[i], seen[i] = bar, map[string]bool{}
		for _, p := range parts {
			seen[i][p.name] = p.has(bar)
		}
		if len([]rune(bar)) > w {
			t.Fatalf("width %d: the bar is %d cells wide:\n%s", w, len([]rune(bar)), s)
		}
		if t.Failed() {
			t.Fatalf("width %d, bar %q, screen:\n%s", w, bar, s)
		}
	}
	// Monotone in width: a part that fits narrow fits wide.
	for i := range widths[:len(widths)-1] {
		for _, p := range parts {
			if seen[i][p.name] && !seen[i+1][p.name] {
				t.Errorf("%s shows at %d columns (%q) but not at %d (%q)",
					p.name, widths[i], bars[i], widths[i+1], bars[i+1])
			}
		}
	}
	for i, w := range widths {
		for j := range parts[:len(parts)-1] {
			if seen[i][parts[j].name] && !seen[i][parts[j+1].name] {
				t.Errorf("width %d: %s shows without %s, which is dropped later: %q",
					w, parts[j].name, parts[j+1].name, bars[i])
			}
		}
		if !seen[i]["keys"] {
			t.Errorf("width %d: the keys hint is the floor and it is gone: %q", w, bars[i])
		}
	}
	// The widest pane shows the whole truth, in order.
	last := len(widths) - 1
	if want := "↑12.3k ↓3.4k · $0.052"; !strings.Contains(bars[last], want) {
		t.Errorf("160 columns: bar = %q, want it to contain %q", bars[last], want)
	}
	if !seen[last]["ctx"] || !seen[last]["model"] {
		t.Errorf("160 columns: context or model missing: %q", bars[last])
	}
}

// A very long session title truncates on the left; it never wraps the
// bar onto a second row and never pushes the composer off screen.
//
// Known, unasserted here: at 80 columns a 200-character title eats the
// whole bar, so the usage chips AND the "? keys" floor are truncated
// away with it — the left side is cut only by the bar's total width,
// never to leave room for the right. The floor is checked at every
// width without a title in TestStatusbarDegradesByWidth.
func TestStatusbarLongTitleTruncates(t *testing.T) {
	t.Parallel()
	title := strings.Repeat("averylongsessiontitle ", 10)[:200]
	hist := statusbarSeed(t, "title.jsonl",
		statusbarEntry("meta", map[string]any{"cwd": "/tmp/demo"}),
		statusbarEntry("input", map[string]any{"text": "name this session"}),
		statusbarEntry("assistant", map[string]any{"text": "```stop\nnamed\n```"}),
		statusbarEntry("title", map[string]any{"text": title}),
		map[string]any{"kind": "done", "data": map[string]any{"usage": statusbarUsage}},
	)
	a := startCfg(t, 80, 24, statusbarCfg(hist))
	s := a.settled()
	ls := strings.Split(s, "\n")
	if r := composerRow(ls); r < 0 || r < len(ls)-3 {
		t.Fatalf("composer not on the last rows (row %d of %d):\n%s", r, len(ls), s)
	}
	bar := statusbarLine(a, s)
	if !strings.Contains(bar, "…") {
		t.Fatalf("the long title is not truncated: %q\nscreen:\n%s", bar, s)
	}
	if !strings.Contains(bar, title[:20]) {
		t.Fatalf("the bar does not start with the title:\nbar %q\nscreen:\n%s", bar, s)
	}
	if strings.Contains(s, title[:120]) {
		t.Fatalf("the whole title reached the screen: it must truncate:\nbar %q\nscreen:\n%s", bar, s)
	}
	if len([]rune(bar)) > 80 {
		t.Fatalf("the bar is %d cells wide in an 80-column pane:\nscreen:\n%s", len([]rune(bar)), s)
	}
	rows := 0
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, title[:20]) {
			rows++
		}
	}
	if rows != 1 {
		t.Fatalf("the title is on %d rows, want 1 (the bar never wraps):\n%s", rows, s)
	}
}

// While a turn streams the right side leads with the spinner and the
// turn's age; when the turn ends both are gone.
func TestStatusbarSpinnerWhileStreaming(t *testing.T) {
	t.Parallel()
	reply := "```stop\n" + strings.TrimSpace(strings.Repeat("slowly streamed words ", 40)) + "\n```"
	tape := statusbarSeed(t, "stream.jsonl",
		statusbarEntry("meta", map[string]any{"cwd": "/tmp/demo"}),
		statusbarEntry("input", map[string]any{"text": "stream me"}),
		statusbarEntry("assistant", map[string]any{"text": reply}),
		statusbarEntry("done", map[string]any{}),
	)
	cfg := fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q, delay_ms: 40}
- id: codemode
  plugin: replay
  config: {file: %q, provide: codemode}
- id: commands
  plugin: commands
- id: history
  plugin: history
- id: loop
  plugin: loop
- id: ui
  plugin: ui
`, tape, tape) + statusbarQuietRows
	a := startCfg(t, 100, 24, cfg)
	a.typeText("stream me")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool {
		for _, l := range strings.Split(s, "\n") {
			if strings.Contains(l, "? keys") && statusbarElapsed.MatchString(l) {
				return true
			}
		}
		return false
	}, "the spinner's elapsed time on the status bar while streaming")
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	s := a.settled()
	a.check("after the turn")
	if bar := statusbarLine(a, s); statusbarElapsed.MatchString(bar) {
		t.Fatalf("the elapsed time is still on the bar after the turn: %q\nscreen:\n%s", bar, s)
	}
}

// A flash takes the right side over — the usage chips give way to it —
// and it expires on the next key, which puts them back.
func TestStatusbarFlashReplacesRightSide(t *testing.T) {
	t.Parallel()
	hist := statusbarSeed(t, "flash.jsonl",
		statusbarEntry("meta", map[string]any{"cwd": "/tmp/demo"}),
		statusbarEntry("input", map[string]any{"text": "spend something"}),
		map[string]any{"kind": "done", "data": map[string]any{"usage": statusbarUsage}},
	)
	a := startCfg(t, 100, 24, statusbarCfg(hist))
	s := a.settled()
	if bar := statusbarLine(a, s); !strings.Contains(bar, "$0.052") {
		t.Fatalf("no usage on the idle bar to displace: %q\nscreen:\n%s", bar, s)
	}
	a.key('x', uv.ModCtrl) // the leader: the bar says it is pending
	a.waitFor("ctrl+x …")
	s = a.settled()
	a.check("flash")
	bar := statusbarLine(a, s)
	if !strings.Contains(bar, "ctrl+x …") {
		t.Fatalf("the flash is not on the bar: %q\nscreen:\n%s", bar, s)
	}
	if strings.Contains(bar, "$0.052") || strings.Contains(bar, "↑12.3k") {
		t.Fatalf("the flash must replace the usage chips, not join them: %q\nscreen:\n%s", bar, s)
	}
	a.key(uv.KeyEsc, 0) // any key ends the flash
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "ctrl+x …") }, "the flash to expire")
	s = a.settled()
	if bar := statusbarLine(a, s); !strings.Contains(bar, "$0.052") {
		t.Fatalf("the usage did not come back after the flash: %q\nscreen:\n%s", bar, s)
	}
}
