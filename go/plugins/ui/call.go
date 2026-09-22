package ui

// Native call rows (go/docs/unreal-engine.md §10.4). On the loop a
// turn's work is a code block and its result, and the per-call events
// the tools plugin emits inside it are detail the web shows under that
// block, so the TUI ignores them. On the engine there is no block: each
// tool call IS the step, and without a row per call the transcript is
// blank for the whole of the work. The two are told apart by the call
// id: the loop numbers its calls per block (an int, a float64 once it
// has been through JSON), the engine keeps the provider's call id (a
// string). A run_js block on the engine is a code block like the loop's,
// and the calls inside it keep their numeric ids and stay detail.
//
// A call's start opens a running row with a spinner and the last
// callTail lines of its live output; its recorded end replaces that row
// in place with one line: what it did, how long, how it ended.

import (
	"fmt"
	"strings"
	"time"
)

// callTail is how many lines of a running call's output its row shows.
const callTail = 3

// callState is a call row's record.
type callState struct {
	id       string // the provider call id: pairs the recorded end with its start
	tool     string
	ms       int64
	exit     *int
	add, del int
	err      string
	failed   bool
	canceled bool
	bg       bool   // adopted as a job when its turn settled without it
	tail     string // live output (call-delta), last callTail lines
}

// nativeCall reports whether a call event is the engine's: see above.
func nativeCall(ev Event) bool {
	_, ok := ev.Data["id"].(string)
	return ok
}

func evStr(data map[string]any, key string) string {
	s, _ := data[key].(string)
	return s
}

// evInt reads a number that may have been through JSON (headless wire,
// a replayed history).
func evInt(data map[string]any, key string) (int64, bool) {
	switch v := data[key].(type) {
	case int:
		return int64(v), true
	case int64:
		return v, true
	case float64:
		return int64(v), true
	}
	return 0, false
}

// callVerb is the word for a tool: the web's CALL_VERBS vocabulary, so
// the two surfaces never name one call two ways.
func callVerb(tool string) string {
	switch tool {
	case "bash":
		return "Ran"
	case "view":
		return "Read"
	case "write":
		return "Wrote"
	case "patch":
		return "Patched"
	case "":
		return "Called"
	}
	return strings.ToUpper(tool[:1]) + tool[1:]
}

func callRowLabel(ev Event) string {
	return strings.TrimSpace(callVerb(evStr(ev.Data, "tool")) + " " + line(ev.Text, 200))
}

func callStateOf(ev Event) *callState {
	s := &callState{id: evStr(ev.Data, "id"), tool: evStr(ev.Data, "tool"), err: evStr(ev.Data, "error")}
	s.ms, _ = evInt(ev.Data, "ms")
	if e, ok := evInt(ev.Data, "exit"); ok {
		n := int(e)
		s.exit = &n
	}
	if n, ok := evInt(ev.Data, "add"); ok {
		s.add = int(n)
	}
	if n, ok := evInt(ev.Data, "del"); ok {
		s.del = int(n)
	}
	s.canceled = ev.Data["canceled"] == true
	s.bg = ev.Data["adopted"] == true
	s.failed = !s.canceled && (s.err != "" || (s.exit != nil && *s.exit != 0))
	return s
}

// callMeta is a finished row's evidence: "1.2s · exit 1", "+3 −1".
// Sub-second calls say "<1s", not "0s", which reads as no time at all.
func callMeta(s *callState) string {
	var parts []string
	if s.ms > 0 {
		if s.ms < 1000 {
			parts = append(parts, "<1s")
		} else {
			parts = append(parts, durText(time.Duration(s.ms)*time.Millisecond))
		}
	}
	if s.exit != nil && *s.exit != 0 {
		parts = append(parts, fmt.Sprintf("exit %d", *s.exit))
	}
	var edit []string
	if s.add > 0 {
		edit = append(edit, fmt.Sprintf("+%d", s.add))
	}
	if s.del > 0 {
		edit = append(edit, fmt.Sprintf("−%d", s.del))
	}
	if len(edit) > 0 {
		parts = append(parts, strings.Join(edit, " "))
	}
	if s.canceled {
		parts = append(parts, "cancelled")
	}
	return strings.Join(parts, " · ")
}

// findCall is the running row for a call id, searched from the end: the
// call that just finished is usually one of the last started.
func (m *model) findCall(id string) *block {
	for i := len(m.blocks) - 1; i >= 0; i-- {
		if b := &m.blocks[i]; b.kind == "call" && b.live && b.call != nil && b.call.id == id {
			return b
		}
	}
	return nil
}

// addCall opens a native call's running row, or closes it in place.
func (m *model) addCall(id int, ev Event) {
	s := callStateOf(ev)
	if evStr(ev.Data, "phase") == "start" {
		m.blocks = append(m.blocks, block{id: id, kind: "call", call: s, label: callRowLabel(ev), live: true})
		return
	}
	b := m.findCall(s.id)
	if b == nil {
		// The end with no start row: a resumed session, or a start
		// that fired before this pane attached.
		m.blocks = append(m.blocks, block{id: id, kind: "call"})
		b = &m.blocks[len(m.blocks)-1]
	}
	b.call, b.label, b.live = s, callRowLabel(ev), false
	// The recorded output replaces the live tail; the row stays one
	// line, and opens onto it.
	b.text = strings.TrimRight(evStr(ev.Data, "output"), "\n")
	b.collapsed = true
}

// addCallDelta feeds a running call's live output to its row.
func (m *model) addCallDelta(ev Event) {
	b := m.findCall(evStr(ev.Data, "id"))
	if b == nil {
		return
	}
	t := b.call.tail + ev.Text
	// Only the tail is kept: a build log would otherwise grow the
	// transcript by megabytes before the record replaces it anyway.
	if lines := strings.Split(t, "\n"); len(lines) > callTail+1 {
		t = strings.Join(lines[len(lines)-callTail-1:], "\n")
	}
	b.call.tail = t
}

// renderCall is one call row: running, a spinner and its output's tail;
// finished, one line that opens onto what it printed.
func (m *model) renderCall(b *block, th theme) string {
	s := b.call
	if s == nil {
		return m.header(b, th)
	}
	st := th["dim"]
	if m.focused(b) {
		st = th["focus"]
	}
	w := max(m.width, 4)
	if b.live {
		out := truncateCols(m.spin.View()+" "+presentVerb(b.label), w)
		var tail []string
		for _, l := range strings.Split(strings.TrimRight(s.tail, "\n"), "\n") {
			if strings.TrimSpace(l) != "" {
				tail = append(tail, l)
			}
		}
		if len(tail) > callTail {
			tail = tail[len(tail)-callTail:]
		}
		for _, l := range tail {
			out += "\n" + th["dim"].Render(truncateCols("    "+sanitizeText(l), w))
		}
		return out
	}
	glyph := "  "
	if b.text != "" {
		glyph = "▾ "
		if b.collapsed {
			glyph = "▸ "
		}
	}
	mark := "✔"
	if s.failed {
		mark = "✗"
	}
	head := glyph + mark + " " + b.label
	if meta := callMeta(s); meta != "" {
		head += " · " + meta
	}
	if s.bg {
		head += " (bg)"
	}
	if s.err != "" && !s.canceled {
		head += " · " + line(s.err, 200)
	}
	if m.focused(b) {
		head = "> " + head // a text marker: focus must not rely on color alone
	}
	if s.failed && !m.focused(b) {
		st = th["error"]
	}
	// One row, like every other closed block: a call that wrapped to
	// three lines stopped being a row.
	head = st.Render(truncateCols(head, w))
	if b.collapsed || b.text == "" {
		return head
	}
	return head + "\n" + m.box(colorDiff(b.text, th), th["result"], th["border"])
}

// presentVerb turns a row label's verb to what is happening now:
// "Ran go test" → "Running go test".
func presentVerb(label string) string {
	for done, now := range map[string]string{"Ran": "Running", "Read": "Reading", "Wrote": "Writing", "Patched": "Patching"} {
		if rest, ok := strings.CutPrefix(label, done+" "); ok {
			return now + " " + rest
		}
		if label == done {
			return now
		}
	}
	return label
}
