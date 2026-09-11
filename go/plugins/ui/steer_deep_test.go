package ui

// Steering, deep, on the ui side: many steers per boundary, steers
// racing the turn's end, slash/bang/ask/paste lines typed mid-turn,
// steers during streaming and subagents, esc with steers pending, and
// a rapid property over random interleavings.
//
//	go test ./plugins/ui -run SteerDeepProp -rapid.checks=2000

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"pgregory.net/rapid"

	"github.com/andreylukin/bough/plugins/history"
)

// steerDeepRec is a steer service the test can flip to refusing (the
// loop's final boundary has passed) and that records what it took.
type steerDeepRec struct {
	ok  bool
	got []string
}

func (s *steerDeepRec) steer(text string) bool {
	if !s.ok {
		return false
	}
	s.got = append(s.got, text)
	return true
}

// steerDeepDrv is a running-turn driver wired to rec, a cancel counter
// and optional extra config.
func steerDeepDrv(t *testing.T, with func(*uiCfg)) (*drv, *steerDeepRec, *int) {
	t.Helper()
	rec := &steerDeepRec{ok: true}
	cancels := 0
	cfg := cfgWith(t, nil, nil, nil)
	cfg.steer = rec.steer
	cfg.cancel = func() { cancels++ }
	if with != nil {
		with(cfg)
	}
	d := newDrv(t, 100, 40, cfg)
	d.typeStr("first")
	d.press(keyEnter())
	if !d.m.running {
		t.Fatal("first prompt did not start a turn")
	}
	return d, rec, &cancels
}

func (d *drv) steerDeepSay(line string) {
	d.typeStr(line)
	d.press(keyEnter())
}

// steerDeepRows lists the user blocks as "text|flags".
func steerDeepRows(d *drv) []string {
	var out []string
	for _, b := range d.m.blocks {
		if b.kind != "user" {
			continue
		}
		s := b.text
		if b.steer {
			s += "|steer"
		}
		if b.pending {
			s += "|pending"
		}
		if b.queued {
			s += "|queued"
		}
		out = append(out, s)
	}
	return out
}

// Two and five steers before the boundary: each goes to the loop
// once, shows pending once; the loop's FIFO "steer" events clear the
// markers one by one, in order; nothing duplicates.
func TestSteerDeepManyBeforeBoundary(t *testing.T) {
	t.Parallel()
	for _, n := range []int{2, 5} {
		t.Run(fmt.Sprint(n), func(t *testing.T) {
			t.Parallel()
			d, rec, _ := steerDeepDrv(t, nil)
			var texts []string
			for i := range n {
				s := fmt.Sprintf("steer number %d", i+1)
				texts = append(texts, s)
				d.steerDeepSay(s)
			}
			if strings.Join(rec.got, "|") != strings.Join(texts, "|") {
				t.Fatalf("steer service got %v, want %v", rec.got, texts)
			}
			if len(d.sent) != 1 {
				t.Fatalf("steers leaked to inputs: %v", d.sent)
			}
			if c := strings.Count(d.plain(), "(steer · pending)"); c != n {
				t.Fatalf("pending rows = %d, want %d:\n%s", c, n, d.plain())
			}
			for i, s := range texts {
				d.event("steer", s)
				rows := steerDeepRows(d)
				for j, want := range texts {
					flag := "|steer"
					if j > i {
						flag += "|pending"
					}
					if rows[j+1] != want+flag {
						t.Fatalf("after landing %d: row %d = %q, want %q", i+1, j, rows[j+1], want+flag)
					}
				}
			}
			d.event("assistant", "steered reply")
			d.event("done", "")
			f := d.plain()
			if strings.Contains(f, "pending") || d.m.running {
				t.Fatalf("running=%v, frame:\n%s", d.m.running, f)
			}
			last := -1
			for _, s := range texts {
				if c := strings.Count(f, s); c != 1 {
					t.Fatalf("%q shown %d times:\n%s", s, c, f)
				}
				i := strings.Index(f, s)
				if i < last {
					t.Fatalf("%q out of order:\n%s", s, f)
				}
				last = i
			}
		})
	}
}

// Two identical steers: the first landing clears exactly one pending
// marker, the second the other — not both, not neither.
func TestSteerDeepIdenticalTexts(t *testing.T) {
	t.Parallel()
	d, _, _ := steerDeepDrv(t, nil)
	d.steerDeepSay("again")
	d.steerDeepSay("again")
	d.event("steer", "again")
	if got := strings.Join(steerDeepRows(d), ","); got != "first,again|steer,again|steer|pending" {
		t.Fatalf("rows after one landing = %s", got)
	}
	d.event("steer", "again")
	if got := strings.Join(steerDeepRows(d), ","); got != "first,again|steer,again|steer" {
		t.Fatalf("rows after two landings = %s", got)
	}
}

// The race at the finish line: the loop took its final boundary, so
// the steer is refused. The line becomes a queued follow-up (never
// dropped), the done starts it as a new turn, and its own done ends
// the spinner.
func TestSteerDeepRefusedAtFinishBecomesNextTurn(t *testing.T) {
	t.Parallel()
	d, rec, _ := steerDeepDrv(t, nil)
	d.event("assistant", "final words")
	rec.ok = false
	d.steerDeepSay("just too late")
	if len(d.sent) != 2 || d.sent[1] != "just too late" {
		t.Fatalf("sent = %v, want the refused steer as input", d.sent)
	}
	if got := strings.Join(steerDeepRows(d), ","); got != "first,just too late|queued" {
		t.Fatalf("rows = %s", got)
	}
	d.event("done", "")
	if !d.m.running {
		t.Fatal("the queued line did not start its own turn at the done")
	}
	d.event("assistant", "second turn")
	d.event("done", "")
	if d.m.running {
		t.Fatal("still running after the second done")
	}
	if f := d.plain(); strings.Count(f, "just too late") != 1 || strings.Contains(f, "(queued)") {
		t.Fatalf("frame:\n%s", f)
	}
}

// A "/" line mid-turn dispatches the command; it is never a steer.
func TestSteerDeepSlashMidTurnDispatches(t *testing.T) {
	t.Parallel()
	d, rec, _ := steerDeepDrv(t, func(c *uiCfg) { c.cmds = reg(t, "alpha") })
	d.typeStr("/alpha x")
	d.m.pal.open = false // typed past the palette: submit the line as-is
	d.press(keyEnter())
	if len(rec.got) != 0 || len(d.sent) != 1 {
		t.Fatalf("slash line steered/sent: steer=%v sent=%v", rec.got, d.sent)
	}
	if f := d.plain(); !strings.Contains(f, "alpha ran x") {
		t.Fatalf("command did not run:\n%s", f)
	}
}

// A "!" line mid-turn runs a shell command; it is never a steer.
func TestSteerDeepBangMidTurnRunsShell(t *testing.T) {
	t.Parallel()
	d, rec, _ := steerDeepDrv(t, nil)
	d.steerDeepSay("!true")
	if len(rec.got) != 0 || len(d.sent) != 1 {
		t.Fatalf("bang line steered/sent: steer=%v sent=%v", rec.got, d.sent)
	}
	ran := false
	for _, b := range d.m.blocks {
		ran = ran || (b.kind == "command" && b.text == "!true")
	}
	if !ran {
		t.Fatalf("bang did not run: %v", steerDeepRows(d))
	}
}

// With an ask pending mid-turn, the line answers the ask: it is not
// also a steer (the model would see it twice).
func TestSteerDeepAskPendingAnswersNotSteers(t *testing.T) {
	t.Parallel()
	fa := &fakeAsk{}
	d, rec, _ := steerDeepDrv(t, func(c *uiCfg) { c.ask = fa })
	d.feed(askEvent())
	d.steerDeepSay("green")
	if len(rec.got) != 0 {
		t.Fatalf("ask answer also steered: %v", rec.got)
	}
	if len(fa.texts) != 1 || fa.texts[0] != "green" {
		t.Fatalf("ask answers = %v", fa.texts)
	}
	// With the ask answered, the next line steers again.
	d.steerDeepSay("now steer")
	if len(rec.got) != 1 || rec.got[0] != "now steer" {
		t.Fatalf("steers = %v", rec.got)
	}
}

// A steer holding a paste placeholder goes to the loop expanded; the
// loop's "steer" event (the expanded text) clears its pending mark.
func TestSteerDeepPastePlaceholder(t *testing.T) {
	t.Parallel()
	d, rec, _ := steerDeepDrv(t, nil)
	body := nLines(30)
	d.typeStr("see: ")
	d.feed(tea.PasteMsg{Content: body})
	if !strings.Contains(d.m.input.Value(), "[Pasted text #1") {
		t.Fatalf("paste did not collapse: %q", d.m.input.Value())
	}
	d.press(keyEnter())
	if len(rec.got) != 1 || rec.got[0] != "see: "+body {
		t.Fatalf("steer got %q, want the expanded paste", rec.got)
	}
	d.event("steer", rec.got[0])
	for _, r := range steerDeepRows(d) {
		if strings.HasSuffix(r, "|pending") {
			t.Fatalf("expanded steer still pending: %v", steerDeepRows(d))
		}
	}
}

// Steers landing while a reply streams and while a subagent card is
// open: the live block settles once, the card survives, each steer
// shows once.
func TestSteerDeepDuringStreamAndSubagent(t *testing.T) {
	t.Parallel()
	d, _, _ := steerDeepDrv(t, nil)
	d.event("assistant-delta", "Spawning a")
	d.steerDeepSay("s-one")
	d.event("assistant-delta", " helper")
	d.event("assistant", "Spawning a helper\n```js\ntools.spawn('x')\n```")
	d.event("code", "tools.spawn('x')")
	d.event("sub:start", "count files")
	d.steerDeepSay("s-two")
	d.event("sub:assistant", "counting")
	d.event("sub:done", "")
	d.event("result", "42")
	d.event("steer", "s-one")
	d.event("steer", "s-two")
	d.event("assistant", "answer 42")
	d.event("done", "")
	f := d.plain()
	for _, s := range []string{"s-one", "s-two", "Spawning a helper", "answer 42"} {
		if c := strings.Count(f, s); c != 1 {
			t.Fatalf("%q shown %d times:\n%s", s, c, f)
		}
	}
	if strings.Contains(f, "pending") || strings.Contains(f, "▌") || d.m.running {
		t.Fatalf("running=%v frame:\n%s", d.m.running, f)
	}
	for _, b := range d.m.blocks {
		if b.live {
			t.Fatalf("live block survived: %+v", b)
		}
	}
}

// Esc with steers pending cancels the turn (the loop records the
// steers under its cancelled note: "steer" events, then cancelled,
// then done). Every row stops pending; none turns into a queued
// follow-up; the spinner stops.
func TestSteerDeepEscWithPendingSteers(t *testing.T) {
	t.Parallel()
	d, _, cancels := steerDeepDrv(t, nil)
	d.steerDeepSay("keep A")
	d.steerDeepSay("keep B")
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if *cancels != 1 {
		t.Fatalf("esc cancelled %d times, want 1", *cancels)
	}
	d.event("steer", "keep A")
	d.event("steer", "keep B")
	d.event("cancelled", "")
	d.event("done", "")
	if d.m.running {
		t.Fatal("still running after the cancelled done")
	}
	if got := strings.Join(steerDeepRows(d), ","); got != "first,keep A|steer,keep B|steer" {
		t.Fatalf("rows = %s", got)
	}
}

// A resumed session puts steers where they were sent: after the block
// result they interrupted, before the reply that followed.
func TestSteerDeepResumeOrder(t *testing.T) {
	t.Parallel()
	e := func(seq int64, kind string, data map[string]any) history.Entry {
		return history.Entry{Seq: seq, Kind: kind, Data: data}
	}
	h := fakeHist{path: "/tmp/s.jsonl", entries: []history.Entry{
		e(1, "input", map[string]any{"text": "go"}),
		e(2, "assistant", map[string]any{"text": "```js\nconsole.log('ONE')\n```"}),
		e(3, "code", map[string]any{"text": "console.log('ONE')"}),
		e(4, "result", map[string]any{"text": "ONE_OUT"}),
		e(5, "input", map[string]any{"text": "steer A", "steer": true}),
		e(6, "input", map[string]any{"text": "steer B", "steer": true}),
		e(7, "assistant", map[string]any{"text": "after steers"}),
		e(8, "done", map[string]any{"text": ""}),
	}}
	d := newDrv(t, 100, 40, cfgWith(t, nil, nil, h))
	f := d.plain()
	iR, iA, iB, iF := strings.Index(f, "ONE_OUT"), strings.Index(f, "❯ steer A (steer)"), strings.Index(f, "❯ steer B (steer)"), strings.Index(f, "after steers")
	if !(iR >= 0 && iR < iA && iA < iB && iB < iF) {
		t.Fatalf("resume order result@%d A@%d B@%d reply@%d:\n%s", iR, iA, iB, iF, f)
	}
}

// Property: any interleaving of steers typed, FIFO landings, stream
// deltas, replies, code/result and subagent events keeps the steer
// bookkeeping exact — the loop got every line once and in order, each
// has one row, pending = sent − landed, and after the final landing
// and done nothing is pending or running.
func TestSteerDeepProp(t *testing.T) {
	run :=func(rt *rapid.T) {
		d, rec, _ := steerDeepDrv(t, nil)
		var sent []string
		landed := 0
		ops := rapid.SliceOfN(rapid.SampledFrom([]string{"steer", "land", "delta", "assistant", "code", "result", "sub:start", "sub:done", "think"}), 1, 40).Draw(rt, "ops")
		for _, op := range ops {
			switch op {
			case "steer":
				s := fmt.Sprintf("st%02d", len(sent))
				sent = append(sent, s)
				d.steerDeepSay(s)
			case "land":
				if landed < len(sent) {
					d.event("steer", sent[landed])
					landed++
				}
			case "delta":
				d.event("assistant-delta", "tok ")
			case "assistant":
				d.event("assistant", "a reply")
			case "code":
				d.event("code", "x()")
			case "result":
				d.event("result", "out")
			case "think":
				d.event("thinking-delta", "hmm")
			default:
				d.event(op, "sub")
			}
			steerDeepPropCheck(rt, d, rec, sent, landed)
		}
		for ; landed < len(sent); landed++ {
			d.event("steer", sent[landed])
		}
		d.event("assistant", "end")
		d.event("done", "")
		steerDeepPropCheck(rt, d, rec, sent, landed)
		if d.m.running {
			rt.Fatal("running after the final done")
		}
		if f := d.plain(); strings.Contains(f, "pending") {
			rt.Fatalf("pending after the final done:\n%s", f)
		}
	}
	rapid.Check(t, run)
}

func steerDeepPropCheck(rt *rapid.T, d *drv, rec *steerDeepRec, sent []string, landed int) {
	if strings.Join(rec.got, ",") != strings.Join(sent, ",") {
		rt.Fatalf("loop got %v, want %v", rec.got, sent)
	}
	if len(d.sent) != 1 {
		rt.Fatalf("a steer went to inputs: %v", d.sent)
	}
	var rows []string
	pending := 0
	for _, b := range d.m.blocks {
		if b.kind == "user" && b.steer {
			rows = append(rows, b.text)
			if b.pending {
				pending++
			}
		}
	}
	if strings.Join(rows, ",") != strings.Join(sent, ",") {
		rt.Fatalf("steer rows %v, want %v", rows, sent)
	}
	if pending != len(sent)-landed {
		rt.Fatalf("pending = %d, want %d", pending, len(sent)-landed)
	}
}
