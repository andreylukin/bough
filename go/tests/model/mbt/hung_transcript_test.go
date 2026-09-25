//go:build !windows

package mbt

import (
	"fmt"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/hung_transcript.fizz against a real serve: the open thread's
// first transcript read and its catch-ups, each of which may answer,
// fail or never answer.
//
// The adapter plays the `selected` effect in web/src/app.tsx one model
// step at a time: the lines it holds, the cursor (the last line's seq),
// the catch-up timer and `paused`. What the server does is real: the
// thread's event stream (its replay on connect and the frames an Event
// records), and every transcript read, a GET made when the page sends it
// whose answer is held until the spec's step. A hang is the network's:
// the read was made and the page never gets its answer.
//
// The abstraction, spec value <- what the adapter sees:
//   - one session per serve, with a finished first turn before any walk.
//     Its frames up to the walk's start are outside the model (the ring
//     replays them to every subscriber: baseEv, as in
//     live_transcript_sync).
//   - Event is one finished turn (llm-control "ok"): its recorded frames
//     reach the open stream and each arms the catch-up, as the page does.
//   - behind is the real lines: an Event recorded after the thread's
//     first read was sent whose entries the page does not hold.
//   - a catch-up out when a newer one is sent is treated as hung: the
//     spec keeps one slot.
type hungTranscriptAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id     string
	turns  int
	baseEv int64 // the stream's last seq before this walk
	event  int64 // the last history seq an Event recorded, 0 before any

	// the page
	open    bool
	stream  *ltsStream
	first   string // none | out | hung | loaded | failed
	firstAt int64  // the newest seq the first read's answer holds
	firstOf []serve.Line
	catchup string // none | out | hung
	catchOf []serve.Line
	lines   []serve.Line
	timer   bool
	paused  bool

	// replayArmsNothing is TestHungTranscriptPathsCatchWrongAdapter's bug:
	// the page ignores what the ring replays when it subscribes.
	replayArmsNothing bool
}

func newHungTranscriptAdapter(t *testing.T) *hungTranscriptAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &hungTranscriptAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	control.Queue(t, a.dir, "t00000", control.Turn{Mode: "ok", Text: "first reply"})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "first turn")
	if err != nil {
		t.Fatal(err)
	}
	a.id = row.ID
	if _, err := waitRow(s, row.ID, "the first turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	if _, err := a.settled(); err != nil {
		t.Fatal(err)
	}
	return a
}

func (a *hungTranscriptAdapter) read(since int64) ([]serve.Line, error) {
	return (&liveTranscriptSyncAdapter{s: a.s}).read(a.id, since)
}

// settled reads the transcript until two reads a quiet apart agree.
func (a *hungTranscriptAdapter) settled() ([]serve.Line, error) {
	return (&liveTranscriptSyncAdapter{s: a.s}).settled(a.id)
}

func lastOf(lines []serve.Line) int64 {
	var n int64
	for _, l := range lines {
		n = max(n, l.Seq)
	}
	return n
}

// Init is a closed thread; everything the stream holds so far is outside
// the model.
func (a *hungTranscriptAdapter) Init() error {
	a.closePage()
	a.gate.reset()
	a.event = 0
	st, err := ltsOpen(a.s, a.id)
	if err != nil {
		return err
	}
	defer st.close()
	fs, _ := st.drain()
	for _, f := range fs {
		a.baseEv = max(a.baseEv, f.ev.Seq)
	}
	return nil
}

func (a *hungTranscriptAdapter) closePage() {
	a.stream.close()
	a.stream = nil
	a.open, a.first, a.firstAt, a.firstOf = false, "none", 0, nil
	a.catchup, a.catchOf, a.lines, a.timer, a.paused = "none", nil, nil, false, false
}

func (a *hungTranscriptAdapter) Cleanup() error {
	a.closePage()
	return nil
}

func (a *hungTranscriptAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Thread", Index: 0}: a}, nil
}

func (a *hungTranscriptAdapter) GetState() (map[string]any, error) {
	a.pump()
	behind := a.open && a.event > a.firstAt && lastOf(a.lines) < a.event
	return map[string]any{
		"open": a.open, "first": a.first, "catchup": a.catchup,
		"timer": a.timer, "behind": behind, "paused": a.paused,
		// The ring's side shows in timer at Open; this is the walk's record.
		"recorded": a.event > 0,
	}, nil
}

// arms says a frame is one the page catches up on: recorded, in the
// model, and not the activity label.
func (a *hungTranscriptAdapter) arms(f ltsFrame) bool {
	return f.ev.Seq > a.baseEv && f.ev.Kind != "activity"
}

// pump takes frames that came between steps: each one the page catches
// up on arms the timer, so one the model has no step for shows.
func (a *hungTranscriptAdapter) pump() {
	if a.stream == nil {
		return
	}
	for {
		select {
		case f, ok := <-a.stream.frames:
			if !ok {
				a.stream = nil
				return
			}
			if a.arms(f) {
				a.timer = true
			}
		default:
			return
		}
	}
}

// load is the effect's start: subscribe (the ring replays), and send
// the first read.
func (a *hungTranscriptAdapter) load() error {
	a.stream.close()
	st, err := ltsOpen(a.s, a.id)
	if err != nil {
		return err
	}
	a.stream = st
	fs, ended := st.drain()
	if ended {
		return fmt.Errorf("the event stream ended during its replay")
	}
	for _, f := range fs {
		if a.arms(f) && !a.replayArmsNothing {
			a.timer = true
		}
	}
	entries, err := a.read(0)
	if err != nil {
		return err
	}
	a.first, a.firstOf, a.firstAt = "out", entries, lastOf(entries)
	return nil
}

func (a *hungTranscriptAdapter) Open() error {
	if !a.gate.pass(!a.open) {
		return nil
	}
	a.open = true
	return a.load()
}

func (a *hungTranscriptAdapter) Leave() error {
	if !a.gate.pass(a.open) {
		return nil
	}
	a.closePage()
	return nil
}

func (a *hungTranscriptAdapter) FirstAnswers() error {
	if !a.gate.pass(a.first == "out") {
		return nil
	}
	// setLines: the read's entries, then any a catch-up brought after them.
	seen := map[int64]bool{}
	for _, e := range a.firstOf {
		seen[e.Seq] = true
	}
	lines := append([]serve.Line(nil), a.firstOf...)
	for _, l := range a.lines {
		if !seen[l.Seq] {
			lines = append(lines, l)
		}
	}
	a.lines, a.first = lines, "loaded"
	return nil
}

func (a *hungTranscriptAdapter) FirstFails() error {
	if !a.gate.pass(a.first == "out") {
		return nil
	}
	a.first = "failed"
	return nil
}

func (a *hungTranscriptAdapter) FirstHangs() error {
	if !a.gate.pass(a.first == "out") {
		return nil
	}
	a.first = "hung"
	return nil
}

// RetryFirst bumps loadTry, which re-runs the whole effect: the old run
// is no longer live, and the new one starts from nothing.
func (a *hungTranscriptAdapter) RetryFirst() error {
	if !a.gate.pass(a.first == "failed") {
		return nil
	}
	a.catchup, a.catchOf, a.lines, a.timer, a.paused = "none", nil, nil, false, false
	return a.load()
}

// Event: a turn runs and records on the open session.
func (a *hungTranscriptAdapter) Event() error {
	a.pump()
	if !a.gate.pass(a.open && !a.timer) {
		return nil
	}
	a.turns++
	name := fmt.Sprintf("t%05d", a.turns)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "reply " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "event "+name); err != nil {
		return err
	}
	fs, err := a.stream.until("the event's done", func(f ltsFrame) bool { return f.ev.Kind == "done" })
	if err != nil {
		return err
	}
	for _, f := range fs {
		if a.arms(f) {
			a.timer = true
		}
	}
	if _, err := waitRow(a.s, a.id, "the event's turn", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	lines, err := a.settled()
	if err != nil {
		return err
	}
	a.event = lastOf(lines)
	return nil
}

func (a *hungTranscriptAdapter) catchUp() error {
	entries, err := a.read(lastOf(a.lines))
	if err != nil {
		return err
	}
	a.catchup, a.catchOf = "out", entries
	return nil
}

func (a *hungTranscriptAdapter) CatchUpFire() error {
	a.pump()
	if !a.gate.pass(a.timer) {
		return nil
	}
	a.timer = false
	return a.catchUp()
}

func (a *hungTranscriptAdapter) CatchUpAnswers() error {
	if !a.gate.pass(a.catchup == "out") {
		return nil
	}
	a.paused = false
	seen := map[int64]bool{}
	for _, l := range a.lines {
		seen[l.Seq] = true
	}
	for _, e := range a.catchOf {
		if !seen[e.Seq] {
			a.lines = append(a.lines, e)
		}
	}
	a.catchup, a.catchOf = "none", nil
	return nil
}

func (a *hungTranscriptAdapter) CatchUpFails() error {
	if !a.gate.pass(a.catchup == "out") {
		return nil
	}
	a.paused, a.timer = true, true
	a.catchup, a.catchOf = "none", nil
	return nil
}

func (a *hungTranscriptAdapter) CatchUpHangs() error {
	if !a.gate.pass(a.catchup == "out") {
		return nil
	}
	a.catchup = "hung"
	return nil
}

func (a *hungTranscriptAdapter) RetryCatchUp() error {
	if !a.gate.pass(a.paused) {
		return nil
	}
	a.timer = false
	return a.catchUp()
}

var hungTranscriptActions = map[string]map[string]fmbt.ActionFunc{"Thread": {
	"Open":           action((*hungTranscriptAdapter).Open),
	"Leave":          action((*hungTranscriptAdapter).Leave),
	"FirstAnswers":   action((*hungTranscriptAdapter).FirstAnswers),
	"FirstFails":     action((*hungTranscriptAdapter).FirstFails),
	"FirstHangs":     action((*hungTranscriptAdapter).FirstHangs),
	"RetryFirst":     action((*hungTranscriptAdapter).RetryFirst),
	"Event":          action((*hungTranscriptAdapter).Event),
	"CatchUpFire":    action((*hungTranscriptAdapter).CatchUpFire),
	"CatchUpAnswers": action((*hungTranscriptAdapter).CatchUpAnswers),
	"CatchUpFails":   action((*hungTranscriptAdapter).CatchUpFails),
	"CatchUpHangs":   action((*hungTranscriptAdapter).CatchUpHangs),
	"RetryCatchUp":   action((*hungTranscriptAdapter).RetryCatchUp),
}}

// hungTranscriptHistory reads the session's transcript: the turn before
// any walk is outside the model, and every later turn is an Event the
// open thread caught up on.
func hungTranscriptHistory(entries []history.Entry) []tracecheck.Step {
	step := func(action string, state map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Thread#0." + action, State: state}
	}
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Thread#0.open": false}}, step("Open", nil), step("FirstAnswers", nil)}
	dones := 0
	for _, e := range entries {
		if e.Kind != "done" {
			continue
		}
		if dones++; dones > 1 {
			steps = append(steps, step("Event", map[string]any{"Thread#0.behind": true}), step("CatchUpFire", nil),
				step("CatchUpAnswers", map[string]any{"Thread#0.behind": false}))
		}
	}
	return steps
}

func init() { historyProjections["hung_transcript"] = hungTranscriptHistory }

func TestHungTranscriptPaths(t *testing.T) {
	t.Parallel()
	paths := loadWalks(t, "hung_transcript", envCover())
	workers := min(4, len(paths))
	for w := range workers {
		t.Run(fmt.Sprintf("serve%d", w), func(t *testing.T) {
			t.Parallel()
			a := newHungTranscriptAdapter(t)
			for j := w; j < len(paths); j += workers {
				if err := walkRole(a, "Thread", hungTranscriptActions, &a.gate, paths[j]); err != nil {
					t.Errorf("path %d: %v", j, err)
				}
			}
			g, err := tracecheck.Load(testdataDir("hung_transcript"))
			if err != nil {
				t.Fatal(err)
			}
			checkHistory(t, g, sessionHistory(t, a.s.Home, a.id), hungTranscriptHistory)
		})
	}
}

// A page that ignores what the ring replays on connect differs only on
// an Open or RetryFirst after an Event, so this walks every link.
func TestHungTranscriptPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newHungTranscriptAdapter(t)
	a.replayArmsNothing = true
	for _, p := range loadWalks(t, "hung_transcript", tracecheck.CoverTransitions) {
		if err := walkRole(a, "Thread", hungTranscriptActions, &a.gate, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with a page that ignores the ring's replay; the walk is not checking state")
}

func TestHungTranscript(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHungTranscriptAdapter(t)
	opts := map[string]any{"max-seq-runs": 300, "max-actions": 10, "max-parallel-runs": 0}
	if err := runMBT(t, "hung_transcript", a, hungTranscriptActions, opts); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The graph folds "the first read was sent before an event that a
// catch-up has since brought in" into the same state as a first read
// sent after it, so the generated walks may take FirstAnswers from that
// state along the harmless route only. This is the other route: a first
// read that replaced the lines would drop the event's entries, and the
// spec says they stay (behind is still false).
func TestHungTranscriptStaleFirstRead(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(testdataDir("hung_transcript"))
	if err != nil {
		t.Fatal(err)
	}
	state := func(first, catchup string, timer, behind bool) map[string]any {
		return map[string]any{"Thread#0.open": true, "Thread#0.first": first, "Thread#0.catchup": catchup,
			"Thread#0.timer": timer, "Thread#0.behind": behind, "Thread#0.paused": false, "Thread#0.recorded": true}
	}
	trace := []tracecheck.Step{
		{Action: "Init", State: map[string]any{"Thread#0.open": false, "Thread#0.recorded": false}},
		{Action: "Thread#0.Open", State: map[string]any{"Thread#0.first": "out", "Thread#0.timer": false, "Thread#0.behind": false}},
		{Action: "Thread#0.Event", State: state("out", "none", true, true)},
		{Action: "Thread#0.CatchUpFire", State: state("out", "out", false, true)},
		{Action: "Thread#0.CatchUpAnswers", State: state("out", "none", false, false)},
		{Action: "Thread#0.FirstAnswers", State: state("loaded", "none", false, false)},
	}
	if v := g.Check(trace); v != nil {
		t.Fatalf("the trace is not a path in the model: %v", v)
	}
	a := newHungTranscriptAdapter(t)
	if err := walkRole(a, "Thread", hungTranscriptActions, &a.gate, tracecheck.Walk{Trace: trace}); err != nil {
		t.Fatal(err)
	}
}
