package vtreal

// A subagent's transcript open in the overlay while the parent keeps
// streaming a turn. The history row resumes a tape whose first turn
// spawned three subagents (tools.spawnAll) with long outputs, so their
// cards render on boot; the llm row streams a separate, word-delayed
// tape, so the parent's deltas keep landing (the gated tape deltas)
// while the reader wheels through subagent 2's transcript. Contract:
//   - model.go refreshOverlay keeps the overlay's offset: a wheel
//     scroll is not undone (no snap to the bottom) by parent output;
//   - the parent's deltas never render into the overlay;
//   - esc returns to the parent at its follow position: bottom, the
//     newest parent output on screen, no "scrolled ↑" cue.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const subagentOverlayScrollWhileParentStreamsLines = 80

// subagentOverlayScrollWhileParentStreamsLine is subagent w's line i.
func subagentOverlayScrollWhileParentStreamsLine(w, i int) string {
	return fmt.Sprintf("W%d-LINE-%03d", w, i)
}

// subagentOverlayScrollWhileParentStreamsWrite writes entries to path.
func subagentOverlayScrollWhileParentStreamsWrite(t *testing.T, path string, entries []map[string]any) {
	t.Helper()
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	for i, e := range entries {
		e["seq"] = i + 1
		e["at"] = time.Date(2026, 9, 10, 12, 0, 0, 0, time.UTC).Add(time.Duration(i) * time.Second).Format(time.RFC3339)
		if err := enc.Encode(e); err != nil {
			t.Fatal(err)
		}
	}
}

// subagentOverlayScrollWhileParentStreamsTapes writes the resumed
// history (three long subagents) and the parent's streaming tape.
func subagentOverlayScrollWhileParentStreamsTapes(t *testing.T, dir string) (hist, parent string) {
	t.Helper()
	e := func(kind string, data map[string]any, parent int) map[string]any {
		m := map[string]any{"kind": kind, "data": data}
		if parent > 0 {
			m["parent"] = parent
		}
		return m
	}
	h := []map[string]any{
		e("meta", map[string]any{"cwd": "/tmp/demo"}, 0),
		e("input", map[string]any{"text": "fan out three readers"}, 0),
		e("assistant", map[string]any{"text": "Spawning three subagents with tools.spawnAll."}, 0),
	}
	for w := 1; w <= 3; w++ {
		var out strings.Builder
		for i := range subagentOverlayScrollWhileParentStreamsLines {
			fmt.Fprintf(&out, "%s output of worker %d\n", subagentOverlayScrollWhileParentStreamsLine(w, i), w)
		}
		st := len(h) + 1
		h = append(h,
			e("sub:start", map[string]any{"worker": w, "text": fmt.Sprintf("read part %d", w)}, 3),
			e("sub:code", map[string]any{"worker": w, "text": fmt.Sprintf("tools.bash(\"cat part%d\")", w)}, st),
			e("sub:result", map[string]any{"worker": w, "text": out.String()}, st+1),
			e("sub:assistant", map[string]any{"worker": w, "text": fmt.Sprintf("Status: ok\nFindings: part %d read.", w)}, st+2),
			e("sub:done", map[string]any{"worker": w, "status": "ok", "steps": 2}, st+3),
		)
	}
	h = append(h,
		e("assistant", map[string]any{"text": "```stop\nAll three subagents finished.\n```"}, 0),
		e("done", map[string]any{"text": ""}, 0),
	)
	hist = filepath.Join(dir, "hist.jsonl")
	subagentOverlayScrollWhileParentStreamsWrite(t, hist, h)

	var body strings.Builder
	for i := range 60 {
		fmt.Fprintf(&body, "PARENT-%02d streaming\n", i)
	}
	body.WriteString("PARENT-END")
	p := []map[string]any{
		e("meta", map[string]any{"cwd": "/tmp/demo"}, 0),
		e("input", map[string]any{"text": "keep talking"}, 0),
		e("assistant", map[string]any{"text": "```stop\n" + body.String() + "\n```"}, 0),
		e("done", map[string]any{"text": ""}, 0),
	}
	parent = filepath.Join(dir, "parent.jsonl")
	subagentOverlayScrollWhileParentStreamsWrite(t, parent, p)
	return hist, parent
}

// subagentOverlayScrollWhileParentStreamsConfig is subagentsConfig on
// the history tape, with the model and runtime answering from the
// parent tape, word-delayed.
func subagentOverlayScrollWhileParentStreamsConfig(hist, parent string) string {
	cfg := subagentsConfig(hist)
	cfg = strings.Replace(cfg,
		fmt.Sprintf("config: {file: %q}\n- id: codemode", hist),
		fmt.Sprintf("config: {file: %q, delay_ms: 60}\n- id: codemode", parent), 1)
	return strings.Replace(cfg,
		fmt.Sprintf("config: {file: %q, provide: codemode}", hist),
		fmt.Sprintf("config: {file: %q, provide: codemode}", parent), 1)
}

var subagentOverlayScrollWhileParentStreamsW2 = regexp.MustCompile(`W2-LINE-(\d{3})`)

// subagentOverlayScrollWhileParentStreamsTop is the first W2 line on
// screen, or -1.
func subagentOverlayScrollWhileParentStreamsTop(s string) int {
	m := subagentOverlayScrollWhileParentStreamsW2.FindStringSubmatch(s)
	if m == nil {
		return -1
	}
	n, _ := strconv.Atoi(m[1])
	return n
}

func TestSubagentOverlayScrollWhileParentStreams(t *testing.T) {
	t.Parallel()
	for _, cols := range []int{60, 120} {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			hist, parent := subagentOverlayScrollWhileParentStreamsTapes(t, t.TempDir())
			a := startCfg(t, cols, 30, subagentOverlayScrollWhileParentStreamsConfig(hist, parent))
			a.waitFor("subagent 3")

			a.typeText("keep talking")
			a.key(uv.KeyEnter, 0)
			a.waitFor("PARENT-")
			subagentsFocusCard(a, "subagent 2")
			if strings.Contains(a.text(), "PARENT-END") {
				t.Fatalf("parent finished before the overlay opened; the tape is too short:\n%s", a.text())
			}

			// Focusing the card scrolled the parent up to it; end parks
			// it back at the bottom, following, before the dive.
			a.key(uv.KeyEnd, 0)
			a.waitUntil(func(string) bool { return scrollingAtBottom(a) }, "parent back at the bottom")
			a.key('o', uv.ModCtrl)
			a.waitFor("esc to close")
			a.waitFor("W2-LINE-")
			scrollingWheel(a, 6, false)
			a.waitUntil(func(s string) bool { return subagentOverlayScrollWhileParentStreamsTop(s) > 0 }, "the overlay to scroll down on wheel")
			top := subagentOverlayScrollWhileParentStreamsTop(a.settled())

			// The parent keeps streaming underneath for a while.
			deadline := time.Now().Add(2 * time.Second)
			for time.Now().Before(deadline) {
				s := a.text()
				if strings.Contains(s, "PARENT-") {
					t.Fatalf("parent delta rendered into the subagent overlay:\n%s", s)
				}
				if !strings.Contains(s, "esc to close") {
					t.Fatalf("overlay closed while the parent streamed:\n%s", s)
				}
				if got := subagentOverlayScrollWhileParentStreamsTop(s); got != top {
					t.Fatalf("overlay moved from W2 line %d to %d while the parent streamed (autofollow after scroll):\n%s", top, got, s)
				}
				time.Sleep(50 * time.Millisecond)
			}
			last := subagentOverlayScrollWhileParentStreamsLine(2, subagentOverlayScrollWhileParentStreamsLines-1)
			if s := a.text(); strings.Contains(s, last) {
				t.Fatalf("overlay snapped to the bottom (%s on screen):\n%s", last, s)
			}

			subagentsEsc(a)
			a.waitUntil(func(s string) bool { return !strings.Contains(s, "esc to close") }, "esc to close the overlay")
			a.waitFor("PARENT-END")
			s := a.settled()
			if strings.Contains(s, "scrolled ↑") {
				t.Errorf("parent not at its follow position after esc:\n%s", s)
			}
			if strings.Contains(s, "W2-LINE-") {
				t.Errorf("subagent transcript leaked into the parent view:\n%s", s)
			}
			a.check("back at the parent")
		})
	}
}
