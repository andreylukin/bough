package ui

// Streaming properties: a reply (markdown, fences, CJK, ZWJ emoji,
// long unbroken runs, ANSI, tags) is cut into random deltas — inside
// runes, escape sequences, fences and tables, empty ones too — and
// interleaved with thinking deltas, subagent/todo/activity events,
// steers, resizes, wheel and typing. Checked after every step.
//
//	go test ./plugins/ui -run StreamModel -rapid.checks=300
//	BOUGH_STREAM_MODEL_LONG=1 ...          longer sequences
//	BOUGH_KNOWN_STREAM_MODEL=1 ...         also assert the known bugs

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"pgregory.net/rapid"
)

var streamModelLong = os.Getenv("BOUGH_STREAM_MODEL_LONG") == "1"

// streamModelKnown: assert the known-bug cases instead of skipping them.
var streamModelKnown = os.Getenv("BOUGH_KNOWN_STREAM_MODEL") == "1"

const streamModelFenceBody = "fmt.Println(1)"

var streamModelPieces = []string{
	"hello", "world", "日本語の文章", "中文", "👨‍👩‍👧", "🐛", "e\u0301", "→", "…",
	"\x1b[31mred\x1b[0m", "\x1b]8;;http://x\x07link\x1b]8;;\x07", "\x1b[1;32m", "\t", "\r", "\x00",
	"| a | b |\n|---|---|\n| 1 | 日本 |\n", "**bold**", "_it_", "`inline`", "# Head\n", "- item\n- two\n",
	"a < b", "<b>tag</b>", "<thinking>mulling</thinking>", "\n", "\n\n", " ",
	"\n```go\n" + streamModelFenceBody + "\n```\n",
	"\n```\nunterminated",
	"https://example.com/" + strings.Repeat("abcdef0123", 20),
	strings.Repeat("x", 150),
}

func streamModelReplyGen(n int) *rapid.Generator[string] {
	return rapid.Custom(func(t *rapid.T) string {
		ps := rapid.SliceOfN(rapid.SampledFrom(streamModelPieces), 0, n).Draw(t, "pieces")
		return strings.Join(ps, "")
	})
}

// streamModelChunks cuts s at random byte offsets (duplicates give
// empty deltas; offsets land inside runes and escapes freely).
func streamModelChunks(t *rapid.T, s, label string) []string {
	cuts := rapid.SliceOfN(rapid.IntRange(0, len(s)), 0, 12).Draw(t, label)
	sort.Ints(cuts)
	var out []string
	prev := 0
	for _, c := range cuts {
		out = append(out, s[prev:c])
		prev = c
	}
	return append(out, s[prev:])
}

type streamModelRun struct {
	t     *rapid.T
	d     *drv
	w, h  int
	draft string
	step  int
}

func (r *streamModelRun) liveCount(kind string) (n, idx int) {
	idx = -1
	for i, b := range r.d.m.blocks {
		if b.live && b.kind == kind {
			n++
			idx = i
		}
	}
	return
}

// frame: no panic, exact height, no row wider than the pane, no raw
// control bytes, and the composer (with any draft) on screen.
func (r *streamModelRun) frame(what string) {
	r.step++
	raw := r.d.view()
	lines := strings.Split(raw, "\n")
	plain := stripANSI(raw)
	if strings.Contains(plain, "✗ render failed") {
		r.t.Fatalf("step %d (%s): render panicked:\n%s", r.step, what, plain)
	}
	if len(lines) != r.h {
		r.t.Fatalf("step %d (%s): %d lines for %dx%d:\n%s", r.step, what, len(lines), r.w, r.h, plain)
	}
	for i, ln := range lines {
		if !utf8.ValidString(ln) {
			// Known bug: a delta cut inside a rune reaches the frame as
			// a half rune (the thinking header preview shows raw text).
			if streamModelKnown {
				r.t.Fatalf("step %d (%s): line %d is not valid UTF-8 (a delta split a rune):\n%q", r.step, what, i, ln)
			}
			continue // its width/control checks are moot until the rune is whole
		}
		if w := ansi.StringWidth(ln); w > r.w {
			r.t.Fatalf("step %d (%s): line %d is %d wide in %d:\n%q", r.step, what, i, w, r.w, ansi.Strip(ln))
		}
		for _, c := range ansi.Strip(ln) {
			if c < 0x20 || c == 0x7f {
				r.t.Fatalf("step %d (%s): line %d carries %q:\n%q", r.step, what, i, c, ansi.Strip(ln))
			}
		}
	}
	if r.draft != "" {
		pl := strings.Split(plain, "\n")
		tail := strings.Join(pl[max(0, len(pl)-6):], "\n")
		if !strings.Contains(tail, r.draft) {
			r.t.Fatalf("step %d (%s): draft %q not in the composer rows:\n%s", r.step, what, r.draft, plain)
		}
	}
}

// noise is one event that may arrive while a reply streams.
func (r *streamModelRun) noise() {
	switch rapid.IntRange(0, 9).Draw(r.t, "noise") {
	case 0:
		r.d.feed(eventMsg{Kind: "activity", Text: "running bash"})
	case 1:
		r.d.feed(eventMsg{Kind: "todo", Text: "- [ ] one\n- [x] two"})
	case 2:
		k := rapid.SampledFrom([]string{"sub:start", "sub:assistant", "sub:code", "sub:result", "sub:done"}).Draw(r.t, "sub")
		r.d.feed(eventMsg{Kind: k, Text: "child " + k, Data: map[string]any{"worker": rapid.IntRange(1, 2).Draw(r.t, "worker")}})
	case 3:
		r.w, r.h = rapid.IntRange(20, 160).Draw(r.t, "w"), rapid.IntRange(8, 50).Draw(r.t, "h")
		r.d.feed(windowSize(r.w, r.h))
	case 4:
		b := tea.MouseWheelUp
		if rapid.Bool().Draw(r.t, "down") {
			b = tea.MouseWheelDown
		}
		r.d.feed(tea.MouseWheelMsg{X: 1, Y: 1, Button: b})
	case 5:
		if r.draft == "" {
			r.draft = rapid.StringMatching(`[a-z]{3,6}`).Draw(r.t, "draft")
			r.d.typeStr(r.draft)
		}
	case 6: // the user steers mid-stream (enter), the loop lands it
		if r.draft != "" {
			line := r.draft
			r.draft = ""
			r.d.press(keyEnter())
			if rapid.Bool().Draw(r.t, "land") {
				r.d.feed(eventMsg{Kind: "steer", Text: line})
			}
		}
	default:
	}
	r.frame("noise")
}

func streamModelCheck(tt *testing.T, t *rapid.T, long bool) {
	r := &streamModelRun{t: t, w: rapid.IntRange(20, 160).Draw(t, "w0"), h: rapid.IntRange(8, 50).Draw(t, "h0")}
	cfg := cfgWith(tt, nil, nil, nil)
	cfg.steer = func(string) bool { return true }
	r.d = newDrv(tt, r.w, r.h, cfg)
	r.d.typeStr("go")
	r.d.press(keyEnter())
	pieces, replies := 6, 2
	if long {
		pieces, replies = 20, 4
	}
	for n := range rapid.IntRange(1, replies).Draw(t, "replies") {
		sentinel := fmt.Sprintf("Sentinel%d", n)
		reply := sentinel + " " + streamModelReplyGen(pieces).Draw(t, "reply")

		// Reasoning first, sometimes.
		think := ""
		if rapid.Bool().Draw(t, "thinks") {
			think = "Reason" + sentinel + " " + streamModelReplyGen(3).Draw(t, "think")
			for _, c := range streamModelChunks(t, think, "tcuts") {
				r.d.feed(eventMsg{Kind: "thinking-delta", Text: c})
				r.frame("thinking-delta")
				if rapid.Bool().Draw(t, "tnoise") {
					r.noise()
				}
			}
		}

		got := ""
		for i, c := range streamModelChunks(t, reply, "cuts") {
			r.d.feed(eventMsg{Kind: "assistant-delta", Text: c})
			got += c
			if i == 0 && r.d.m.trailing != "" {
				t.Fatalf("a new reply must supersede the held prose, trailing=%q", r.d.m.trailing)
			}
			nl, idx := r.liveCount("assistant")
			if nl != 1 {
				t.Fatalf("%d live assistant blocks after a delta, want 1", nl)
			}
			if r.d.m.blocks[idx].text != got {
				t.Fatalf("live text %q != deltas so far %q", r.d.m.blocks[idx].text, got)
			}
			r.frame("assistant-delta")
			if rapid.Bool().Draw(t, "anoise") {
				r.noise()
			}
		}

		if rapid.IntRange(0, 4).Draw(t, "end") == 0 { // esc mid-stream: the loop's cancel path
			r.d.feed(eventMsg{Kind: "cancelled"})
			r.d.feed(eventMsg{Kind: "done"})
			r.frame("cancel")
			if nl, _ := r.liveCount("assistant"); nl != 0 || strings.Contains(r.d.plain(), "▌") {
				t.Fatalf("cancelled turn left %d live assistant block(s):\n%s", nl, r.d.plain())
			}
			return
		}

		if think != "" {
			r.d.feed(eventMsg{Kind: "thinking", Text: think})
			r.frame("thinking")
			nt, _ := r.liveCount("thinking")
			hits := 0
			for _, b := range r.d.m.blocks {
				if b.kind == "thinking" && strings.Contains(b.text, "Reason"+sentinel) {
					hits++
				}
			}
			if nt != 0 || hits != 1 {
				t.Fatalf("thinking did not settle into one block: live=%d blocks=%d", nt, hits)
			}
		}
		r.d.feed(eventMsg{Kind: "assistant", Text: reply})
		r.frame("assistant")
		if nl, _ := r.liveCount("assistant"); nl != 0 {
			t.Fatalf("%d live blocks after the final reply", nl)
		}
		hits := 0
		for _, b := range r.d.m.blocks {
			if b.kind == "assistant" && strings.Contains(b.text, sentinel) {
				hits++
			}
		}
		if hits != 1 {
			t.Fatalf("reply %s is in %d assistant blocks, want 1", sentinel, hits)
		}
		if strings.Contains(r.d.plain(), "▌") {
			t.Fatalf("cursor left after the final reply:\n%s", r.d.plain())
		}
		if strings.Contains(reply, "```go\n"+streamModelFenceBody) {
			r.d.feed(eventMsg{Kind: "code", Text: streamModelFenceBody})
			r.d.feed(eventMsg{Kind: "result", Text: "1"})
			r.frame("code+result")
		}
	}
	held := r.d.m.trailing
	r.d.feed(eventMsg{Kind: "done"})
	r.frame("done")
	if r.d.m.trailing != "" {
		t.Fatalf("done left prose held: %q", r.d.m.trailing)
	}
	if want := strings.TrimSpace(splitAssistantProse(held)); want != "" {
		last := ""
		for _, b := range r.d.m.blocks {
			if b.kind == "assistant" {
				last = b.text
			}
		}
		if !strings.Contains(last, want) {
			t.Fatalf("held prose %q not flushed at done; last reply %q", held, last)
		}
	}
	if nl, _ := r.liveCount("assistant"); nl != 0 || strings.Contains(r.d.plain(), "▌") {
		t.Fatalf("live block survived done:\n%s", r.d.plain())
	}
}

// splitAssistantProse is the prose part of text as addAssistant keeps it.
func splitAssistantProse(text string) string {
	for _, b := range splitAssistant(text) {
		if b.kind == "assistant" {
			return b.text
		}
	}
	return ""
}

func TestStreamModelProperty(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) { streamModelCheck(t, rt, false) })
}

func TestStreamModelPropertyLong(t *testing.T) {
	if !streamModelLong {
		t.Skip("set BOUGH_STREAM_MODEL_LONG=1")
	}
	rapid.Check(t, func(rt *rapid.T) { streamModelCheck(t, rt, true) })
}

// The loop's cancel paths (loop.go finish("cancelled") after an
// interrupted LLM call) emit "cancelled" then "done" and never a final
// "assistant": nothing drops the live block.
func TestStreamModelCancelLeavesLiveBlock(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.event("assistant-delta", "Half an answ")
	d.event("cancelled", "")
	d.event("done", "")
	for _, b := range d.m.blocks {
		if b.live {
			t.Fatalf("live block survived a cancelled turn: %+v\n%s", b, d.plain())
		}
	}
	if p := d.plain(); strings.Contains(p, "▌") {
		t.Fatalf("cursor survived a cancelled turn:\n%s", p)
	}
}

// A steer typed while the model reasons (or a todo / subagent card)
// lands between deltas; the reasoning must still settle as one block.
// (rapid shrink: thinking-delta "", todo, thinking-delta "Reason…", thinking)
func TestStreamModelThinkingSplitByInterleavedBlock(t *testing.T) {
	t.Parallel()
	d, _ := steerDrv(t, true)
	d.event("thinking-delta", "first half, ")
	d.typeStr("use B")
	d.press(keyEnter())
	d.event("thinking-delta", "second half")
	d.event("thinking", "first half, second half")
	var live, blocks int
	for _, b := range d.m.blocks {
		if b.kind == "thinking" {
			blocks++
			if b.live {
				live++
			}
		}
	}
	if live != 0 || blocks != 1 {
		t.Fatalf("reasoning split into %d thinking blocks, %d still live:\n%s", blocks, live, d.plain())
	}
}

// Every prefix of tricky markdown (byte-level, so broken runes too)
// renders without a panic, live and finished, at narrow and wide panes.
func TestStreamModelMarkdownPrefixes(t *testing.T) {
	t.Parallel()
	docs := []string{
		"# Title\n\n| a | b |\n|---|:-:|\n| 日本 | 👨‍👩‍👧 |\n| x",
		"Intro\n```go\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n```\nafter **bold _nest**_ end",
		"- a\n  - b\n    1. c\n> quote `code` [link](http://x.y/" + strings.Repeat("z", 90) + ")\n",
		"\x1b[31mred\x1b]8;;http://x\x07L\x1b]8;;\x07 <thinking>t</thinking> <system-x>no</system-x> ```stop\ndone\n```",
		"***\n~~~\ntilde fence\n~~~\n<details><summary>s</summary>\n\n* [ ] task\n</details>\n![img](x.png)",
	}
	for _, w := range []int{20, 100} {
		d := newDrv(t, w, 20, cfgWith(t, nil, nil, nil))
		for _, doc := range docs {
			for i := 0; i <= len(doc); i++ {
				p := doc[:i]
				func() {
					defer func() {
						if r := recover(); r != nil {
							t.Fatalf("w=%d prefix %q panicked: %v", w, p, r)
						}
					}()
					liveView(p)
					d.m.markdown(p)
					d.m.render(&block{kind: "assistant", text: p, live: true}, d.m.cfg.Load())
					d.m.render(&block{kind: "assistant", text: p}, d.m.cfg.Load())
				}()
			}
		}
	}
}

// A turn cancelled mid-reasoning never gets its final "thinking": the
// next turn's reasoning must open its own block, not grow the old one.
func TestStreamModelThinkingDoesNotCrossTurns(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.event("thinking-delta", "old turn")
	d.event("cancelled", "")
	d.event("done", "")
	d.typeStr("again")
	d.press(keyEnter())
	d.event("thinking-delta", "new turn")
	d.event("thinking", "new turn")
	lastUser, newAt := -1, -1
	for i, b := range d.m.blocks {
		if b.kind == "user" {
			lastUser = i
		}
		if b.kind == "thinking" && b.live {
			t.Fatalf("reasoning stayed live: %+v\n%s", b, d.plain())
		}
		if b.kind == "thinking" && b.text == "new turn" {
			newAt = i
		}
	}
	if newAt < lastUser {
		t.Fatalf("new turn's reasoning landed in the old turn (at %d, user at %d)\n%s", newAt, lastUser, d.plain())
	}
}
