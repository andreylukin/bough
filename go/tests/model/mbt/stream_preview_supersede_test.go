//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"reflect"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/stream_preview_supersede.fizz against a real serve. The engine's
// half is real: session #0's child runs one held turn on llm-control,
// whose held request streams each fragment the walk asks for (Think,
// Stream), starts a new attempt (Reset) and answers with exactly the
// runs streamed (a release's Items), and every recorded entry is read
// back from the transcript. The page's half (the preview, `superseded`,
// the catch-up timer and its drop) is the adapter, doing what the
// `selected` effect and streamAfter in web/src/app.tsx do with the raw
// SSE frames the server sends, one model step at a time; the browser
// stage checks the page itself.
//
// The abstraction, spec value <- what the adapter sees:
//   - fragment f is the text "f<f> ", streamed as one delta; a frame's
//     fragments are the f<k> tokens in its text, an entry's in its text.
//   - cur is the runs the deltas since the last response built, as the
//     engine merges them; a delta-reset frame clears it, a response
//     (Respond, Stop) takes it.
//   - disk is the entries after the walk's baseline (the prompt and
//     everything before the held request) of the kinds the spec has:
//     thinking, assistant (partial from data.partial), the steer's input,
//     the call a response makes to go on, done. "cancelled" is folded
//     into Stop's done as the spec does; anything else unexpected is
//     reported under its own kind, where it is a mismatch.
//   - Respond answers with the runs of cur and, unless a steer is queued
//     to ask the model again, one bash call ("true"), whose end is
//     recorded: the only other ways a turn goes on past a response.
//   - Finish answers the held request with nothing; with a steer queued
//     the request the steer makes answers with nothing too.
//   - cut is always false: nothing on the page reads data.partial.

// sppFrag matches a fragment's token in a delta's or an entry's text.
var sppFrag = regexp.MustCompile(`\bf(\d+)\b`)

func sppFrags(text string) []int {
	var out []int
	for _, m := range sppFrag.FindAllStringSubmatch(text, -1) {
		n, _ := strconv.Atoi(m[1])
		out = append(out, n)
	}
	return out
}

func sppText(frags []int) string {
	var b strings.Builder
	for _, f := range frags {
		fmt.Fprintf(&b, "f%d ", f)
	}
	return b.String()
}

type sppRun struct {
	kind  string
	frags []int
}

// sppAfter is streamAfter (and, with sealed 0, the engine's own merge):
// a fragment extends the last run of its kind unless that run is sealed.
func sppAfter(runs []sppRun, sealed int, kind string, f int) []sppRun {
	n := len(runs)
	if n > sealed && runs[n-1].kind == kind {
		out := slices.Clone(runs)
		out[n-1] = sppRun{kind, append(slices.Clone(runs[n-1].frags), f)}
		return out
	}
	return append(slices.Clone(runs), sppRun{kind, []int{f}})
}

func sppRuns(rs []sppRun) []any {
	out := []any{}
	for _, r := range rs {
		out = append(out, map[string]any{"kind": r.kind, "frags": ints(r.frags)})
	}
	return out
}

func ints(xs []int) []any {
	out := []any{}
	for _, x := range xs {
		out = append(out, x)
	}
	return out
}

type streamPreviewSupersedeAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string
	gate  gate
	ids   []string
	walks map[string][]string // each session's walk, by id, for the trace check's errors
	turn  int                 // names held requests uniquely across walks

	// the engine, this walk's
	id      string
	held    string // the held request's llm-control turn
	base    int64  // last transcript seq before the walk
	n       int
	cur     []sppRun
	done    bool
	steered bool
	queued  bool

	// the page
	stream *ltsStream
	runs   []sppRun
	sup    int
	lines  []serve.Line // after base, as the catch-ups appended them
	timer  bool
	fetch  bool
	drop   int

	// The deliberate bugs the wrong-adapter tests inject. dropAtLanding:
	// the catch-up slices by `superseded` as it is when the response
	// lands instead of as it was when the request started, which only a
	// record landing mid-catch-up shows (the path walk). keepOnReset: the
	// page ignores a delta-reset, which two steps show (the random runs).
	dropAtLanding, keepOnReset bool
}

func newStreamPreviewSupersedeAdapter(t *testing.T) *streamPreviewSupersedeAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &streamPreviewSupersedeAdapter{t: t, s: s, dir: control.Dir(s.Home), walks: map[string][]string{}}
}

// Init starts a walk. Its session is made at the walk's first step that
// is taken (see liveTranscriptSyncAdapter.Init): the initial state is as
// true of a session not made yet.
func (a *streamPreviewSupersedeAdapter) Init() error {
	a.stream.close()
	*a = streamPreviewSupersedeAdapter{t: a.t, s: a.s, dir: a.dir, ids: a.ids, walks: a.walks, turn: a.turn, dropAtLanding: a.dropAtLanding, keepOnReset: a.keepOnReset}
	a.gate.reset()
	return nil
}

func (a *streamPreviewSupersedeAdapter) nextTurn() string {
	a.turn++
	return fmt.Sprintf("p%05d", a.turn)
}

// step is each action's preamble: frames that came between steps, the
// spec's `require` through the gate, and the session on first use.
func (a *streamPreviewSupersedeAdapter) step(enabled bool) (bool, error) {
	a.pump()
	if !a.gate.pass(enabled) {
		return false, nil
	}
	if a.id == "" {
		if err := a.prepare(); err != nil {
			return false, err
		}
	}
	return true, nil
}

// prepare makes the session with its first request held and nothing
// streamed, and opens the page on it: its first load and the catch-up
// its replay armed are over, so the walk starts from a quiet page.
func (a *streamPreviewSupersedeAdapter) prepare() error {
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block"})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, name
	a.ids = append(a.ids, row.ID)
	if err := a.s.Prompt(ctx, row.ID, "turn "+name); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	var prev []serve.Line
	for i := 0; ; i++ {
		lines, err := a.read(0)
		if err != nil {
			return err
		}
		if prev != nil && len(lines) == len(prev) {
			for _, l := range lines {
				a.base = max(a.base, l.Seq)
			}
			break
		}
		if i == 100 {
			return errors.New("the transcript never settled")
		}
		prev = lines
		time.Sleep(ltsQuiet)
	}
	st, err := ltsOpen(a.s, a.id)
	if err != nil {
		return err
	}
	a.stream = st
	if _, ended := st.drain(); ended {
		return errors.New("the event stream ended during its replay")
	}
	return nil
}

// read is GET /api/sessions/{id}?since=: the page's api.session.
func (a *streamPreviewSupersedeAdapter) read(since int64) ([]serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("%s/api/sessions/%s?since=%d", a.s.URL, url.PathEscape(a.id), since), nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET %s: %s", a.id, resp.Status)
	}
	var r struct {
		Entries []serve.Line `json:"entries"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return nil, err
	}
	return r.Entries, nil
}

// pump applies the frames the stream brought outside any step.
func (a *streamPreviewSupersedeAdapter) pump() {
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
			a.apply(f.ev)
		default:
			return
		}
	}
}

// apply is the page's subscribe callback for one frame, and the engine's
// merge of a fragment into cur.
func (a *streamPreviewSupersedeAdapter) apply(ev serve.Event) {
	switch ev.Kind {
	case "activity", "call-delta":
		return
	case "call", "sub:call":
		if _, native := ev.Extra["id"].(string); native && ev.Extra["phase"] == "start" {
			return
		}
	case "assistant-delta", "thinking-delta":
		kind := "assistant"
		if ev.Kind == "thinking-delta" {
			kind = "thinking"
		}
		for _, f := range sppFrags(ev.Text) {
			a.runs = sppAfter(a.runs, a.sup, kind, f)
			a.cur = sppAfter(a.cur, 0, kind, f)
			a.n = max(a.n, f)
		}
		return
	case "delta-reset":
		a.cur = nil
		if !a.keepOnReset {
			a.runs, a.sup = nil, 0
		}
		return
	}
	a.sup = len(a.runs)
	a.timer = true
}

// until applies frames until one satisfies ok, then the rest of the burst.
func (a *streamPreviewSupersedeAdapter) until(what string, ok func(serve.Event) bool) error {
	if a.stream == nil {
		return fmt.Errorf("no event stream waiting for %s", what)
	}
	fs, err := a.stream.until(what, func(f ltsFrame) bool { return ok(f.ev) })
	for _, f := range fs {
		a.apply(f.ev)
	}
	return err
}

// quiet applies frames until the stream has been silent for ltsQuiet.
func (a *streamPreviewSupersedeAdapter) quiet() {
	if a.stream == nil {
		return
	}
	fs, _ := a.stream.drain()
	for _, f := range fs {
		a.apply(f.ev)
	}
}

// settle waits until two transcript reads a quiet apart agree, applying
// the frames meanwhile: a closed turn's summary and title land after its
// done.
func (a *streamPreviewSupersedeAdapter) settle() error {
	prev := -1
	for range 100 {
		lines, err := a.read(a.base)
		if err != nil {
			return err
		}
		if len(lines) == prev {
			return nil
		}
		prev = len(lines)
		a.quiet()
	}
	return errors.New("the transcript never settled")
}

// Cleanup closes a walk's turn the way the spec can (Finish between
// requests, Stop mid-stream), so its transcript stays a path in the
// model, and archives the session, which ends its child: a serve walks
// hundreds of them.
func (a *streamPreviewSupersedeAdapter) Cleanup() error {
	defer func() {
		a.stream.close()
		a.stream = nil
	}()
	if a.id == "" {
		return nil
	}
	if !a.done {
		a.gate.reset()
		var err error
		if len(a.cur) == 0 {
			err = a.Finish()
		} else {
			err = a.Stop()
		}
		if err != nil {
			return err
		}
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	return err
}

func (a *streamPreviewSupersedeAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Preview", Index: 0}: a}, nil
}

// entry is the spec's record for one transcript line, or nil when the
// spec folds it away.
func sppEntry(kind, text string, data map[string]any) map[string]any {
	rec := func(k string, frags []int, partial bool) map[string]any {
		return map[string]any{"kind": k, "frags": ints(frags), "partial": partial}
	}
	switch kind {
	case "thinking":
		return rec(kind, sppFrags(text), false)
	case "assistant":
		return rec(kind, sppFrags(text), data["partial"] == true)
	case "input":
		if data["steer"] == true {
			return rec("input", nil, false)
		}
	case "call", "done":
		return rec(kind, nil, false)
	case "cancelled", "turn-summary", "title", "hook":
		// Stop's cancelled, the records hooks leave around a call, and
		// what a closed turn's done brings after it.
		return nil
	}
	return rec(kind, nil, false) // not the spec's: a mismatch
}

func (a *streamPreviewSupersedeAdapter) GetState() (map[string]any, error) {
	a.pump()
	disk := []any{}
	turn := "running"
	if a.id != "" {
		ctx, cancel := actionCtx()
		row, lines, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return nil, err
		}
		for _, l := range lines {
			if l.Seq <= a.base {
				continue
			}
			if e := sppEntry(l.Kind, l.Text, l.Data); e != nil {
				disk = append(disk, e)
			}
		}
		switch row.Status {
		case serve.StatusRunning:
		case serve.StatusDone, serve.StatusStopped:
			turn = "done"
		default:
			turn = string(row.Status)
		}
	}
	seen := 0
	for _, l := range a.lines {
		if sppEntry(l.Kind, l.Text, l.Data) != nil {
			seen++
		}
	}
	header := ""
	if n := len(a.runs); n > 0 {
		header = "Working"
		if a.runs[n-1].kind == "thinking" {
			header = "Thinking"
		}
	}
	return map[string]any{
		"cur": sppRuns(a.cur), "disk": disk, "n": a.n, "turn": turn, "steered": a.steered, "queued": a.queued,
		"stream": sppRuns(a.runs), "sup": a.sup, "seen": seen, "timer": a.timer, "fetch": a.fetch,
		"drop": a.drop, "header": header, "cut": false,
	}, nil
}

// --- the engine ---

func (a *streamPreviewSupersedeAdapter) delta(kind string) error {
	if ok, err := a.step(!a.done && a.n < 2); !ok {
		return err
	}
	f := a.n + 1
	if kind == "thinking" {
		control.Think(a.t, a.dir, a.held, sppText([]int{f}), actionTimeout)
	} else {
		control.Stream(a.t, a.dir, a.held, sppText([]int{f}), actionTimeout)
	}
	want := kind + "-delta"
	if kind == "text" {
		want = "assistant-delta"
	}
	return a.until("fragment "+strconv.Itoa(f), func(ev serve.Event) bool {
		return ev.Kind == want && slices.Contains(sppFrags(ev.Text), f)
	})
}

func (a *streamPreviewSupersedeAdapter) ThinkingDelta() error { return a.delta("thinking") }
func (a *streamPreviewSupersedeAdapter) TextDelta() error     { return a.delta("text") }

// Respond answers the held request with the runs streamed, as a provider
// response holding them, and holds the request that follows.
func (a *streamPreviewSupersedeAdapter) Respond(summary bool) error {
	// The spec offers summary=False only when there is thinking to leave
	// unsummarised.
	thinks := slices.ContainsFunc(a.cur, func(r sppRun) bool { return r.kind == "thinking" })
	if ok, err := a.step(!a.done && len(a.cur) > 0 && (summary || thinks)); !ok {
		return err
	}
	items := []control.Item{}
	for _, r := range a.cur {
		kind := r.kind
		if kind == "assistant" {
			kind = "text"
		}
		items = append(items, control.Item{Kind: kind, Text: sppText(r.frags), Summary: summary})
	}
	next := a.nextTurn()
	control.Queue(a.t, a.dir, next, control.Turn{Mode: "block"})
	release := control.Turn{Items: items}
	if !a.queued {
		release.Calls = []control.Call{{Name: "bash", Args: map[string]any{"command": "true"}}}
	}
	control.ReleaseWith(a.t, a.dir, a.held, release)
	a.held = next
	control.WaitTaken(a.t, a.dir, next, actionTimeout)
	a.quiet()
	a.cur, a.queued = nil, false
	return nil
}

func (a *streamPreviewSupersedeAdapter) DeltaReset() error {
	if ok, err := a.step(!a.done && len(a.cur) > 0); !ok {
		return err
	}
	control.Reset(a.t, a.dir, a.held, actionTimeout)
	return a.until("the delta-reset", func(ev serve.Event) bool { return ev.Kind == "delta-reset" })
}

func (a *streamPreviewSupersedeAdapter) Steer() error {
	if ok, err := a.step(!a.done && !a.steered && len(a.cur) > 0 && a.n == 1); !ok {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "steer "+a.held); err != nil {
		return err
	}
	a.steered, a.queued = true, true
	return a.until("the steer", func(ev serve.Event) bool { return ev.Kind == "steer" })
}

func (a *streamPreviewSupersedeAdapter) Stop() error {
	if ok, err := a.step(!a.done && len(a.cur) > 0); !ok {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	if err := a.until("the stopped turn's done", func(ev serve.Event) bool { return ev.Kind == "done" }); err != nil {
		return err
	}
	a.done, a.cur, a.queued = true, nil, false
	return a.settle()
}

func (a *streamPreviewSupersedeAdapter) Finish() error {
	if ok, err := a.step(!a.done && len(a.cur) == 0); !ok {
		return err
	}
	if a.queued {
		control.Queue(a.t, a.dir, a.nextTurn(), control.Turn{Mode: "ok"})
	}
	control.Release(a.t, a.dir, a.held)
	if err := a.until("the turn's done", func(ev serve.Event) bool { return ev.Kind == "done" }); err != nil {
		return err
	}
	a.done, a.queued = true, false
	return a.settle()
}

// --- the page ---

func (a *streamPreviewSupersedeAdapter) CatchUpFire() error {
	if ok, err := a.step(a.timer && !a.fetch); !ok {
		return err
	}
	a.timer, a.fetch, a.drop = false, true, a.sup
	return nil
}

func (a *streamPreviewSupersedeAdapter) cursor() int64 {
	if len(a.lines) == 0 {
		return a.base
	}
	return a.lines[len(a.lines)-1].Seq
}

func (a *streamPreviewSupersedeAdapter) CatchUpOk() error {
	if ok, err := a.step(a.fetch); !ok {
		return err
	}
	entries, err := a.read(a.cursor())
	if err != nil {
		return err
	}
	d := a.drop
	if a.dropAtLanding {
		d = a.sup
	}
	a.fetch, a.drop = false, 0
	if len(entries) == 0 {
		return nil
	}
	a.sup = max(0, a.sup-d)
	a.runs = a.runs[min(d, len(a.runs)):]
	seen := map[int64]bool{}
	for _, l := range a.lines {
		seen[l.Seq] = true
	}
	for _, e := range entries {
		if !seen[e.Seq] {
			a.lines = append(a.lines, e)
		}
	}
	return nil
}

func (a *streamPreviewSupersedeAdapter) CatchUpFails() error {
	if ok, err := a.step(a.fetch); !ok {
		return err
	}
	a.fetch, a.drop, a.timer = false, 0, true
	return nil
}

// respondArg reads the spec's `oneof summary` off the runner's args.
func respondArg(m any, args []fmbt.Arg) (any, error) {
	summary := true
	for _, arg := range args {
		if v, ok := arg.Value.(bool); ok && arg.Name == "summary" {
			summary = v
		}
	}
	return nil, m.(*streamPreviewSupersedeAdapter).Respond(summary)
}

var streamPreviewSupersedeActions = map[string]map[string]fmbt.ActionFunc{"Preview": {
	"ThinkingDelta": action((*streamPreviewSupersedeAdapter).ThinkingDelta),
	"TextDelta":     action((*streamPreviewSupersedeAdapter).TextDelta),
	"Respond":       respondArg,
	"DeltaReset":    action((*streamPreviewSupersedeAdapter).DeltaReset),
	"Steer":         action((*streamPreviewSupersedeAdapter).Steer),
	"Stop":          action((*streamPreviewSupersedeAdapter).Stop),
	"Finish":        action((*streamPreviewSupersedeAdapter).Finish),
	"CatchUpFire":   action((*streamPreviewSupersedeAdapter).CatchUpFire),
	"CatchUpOk":     action((*streamPreviewSupersedeAdapter).CatchUpOk),
	"CatchUpFails":  action((*streamPreviewSupersedeAdapter).CatchUpFails),
}, "": {
	// deadlock_detection is off, so the runner offers a role-less "end"
	// in every state (see unseenTroubleAckActions); taken, it matches no
	// link, so it declines like a disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*streamPreviewSupersedeAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

// The runner draws actions at random among all ten and a walk ends at
// its first disabled one; the session is made lazily, so the many
// one-step walks cost nothing. SPS_SEQ_RUNS overrides.
func streamPreviewSupersedeOptions() map[string]any {
	runs := 300
	if n, err := strconv.Atoi(os.Getenv("SPS_SEQ_RUNS")); err == nil && n > 0 {
		runs = n
	}
	return map[string]any{"max-seq-runs": runs, "max-actions": 10, "max-parallel-runs": 0}
}

func TestStreamPreviewSupersede(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newStreamPreviewSupersedeAdapter(t)
	if err := runMBT(t, "stream_preview_supersede", a, streamPreviewSupersedeActions, streamPreviewSupersedeOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "stream_preview_supersede"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), streamPreviewSupersedeHistory)
	}
}

// A page that ignores a delta-reset must fail the run: a fragment and
// the reset are one walk in about sixty, and 300 walks all miss it with
// odds under 1%.
func TestStreamPreviewSupersedeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newStreamPreviewSupersedeAdapter(t)
	a.keepOnReset = true
	if err := runMBT(t, "stream_preview_supersede", a, streamPreviewSupersedeActions, streamPreviewSupersedeOptions()); err == nil {
		t.Fatal("a run whose page ignores a delta-reset passed; the runner is not checking state")
	}
}

// walk drives one generated path from Init and compares the role state
// after every step. Every action on a path is enabled in the spec, so
// the gate closing is itself a mismatch.
func (a *streamPreviewSupersedeAdapter) walk(p genPath) (err error) {
	var names []string
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
		if a.id != "" {
			a.walks[a.id] = names
		}
	}()
	for i, st := range p.Trace {
		name := strings.TrimPrefix(st.Action, "Preview#0.")
		names = append(names, name)
		want := sppRole(st.State)
		switch {
		case st.Action == "Init":
			if i > 0 {
				if err := a.Cleanup(); err != nil {
					return fmt.Errorf("%v: %w", names, err)
				}
			}
			err = a.Init()
		case name == "end":
			// fizz's self-link where the spec's bounds are used up (a
			// done turn with the page quiet): nothing happens.
		case name == "Respond":
			err = a.Respond(sppSummary(sppRole(p.Trace[i-1].State), want))
		default:
			f, ok := streamPreviewSupersedeActions["Preview"][name]
			if !ok {
				return fmt.Errorf("%v: no action %q", names, st.Action)
			}
			_, err = f(a, nil)
		}
		if err == nil && a.gate.off {
			err = errors.New("the adapter found the action disabled")
		}
		if err != nil {
			return fmt.Errorf("%v: %w", names, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("%v: %w", names, err)
		}
		g := jsonRound(got).(map[string]any)
		for k, w := range jsonRound(want).(map[string]any) {
			if !reflect.DeepEqual(g[k], w) {
				wj, _ := json.Marshal(w)
				hj, _ := json.Marshal(g[k])
				return fmt.Errorf("%v: %s: want %s, got %s", names, k, wj, hj)
			}
		}
	}
	return nil
}

// sppRole is a path state's Preview#0 fields by bare name.
func sppRole(s map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range s {
		if f, ok := strings.CutPrefix(k, "Preview#0."); ok {
			out[f] = v
		}
	}
	return out
}

// sppSummary is Respond's branch on a path: whether the response's
// thinking was summarised, i.e. recorded.
func sppSummary(before, after map[string]any) bool {
	count := func(s map[string]any) int {
		n := 0
		disk, _ := s["disk"].([]any)
		for _, e := range disk {
			if m, _ := e.(map[string]any); m["kind"] == "thinking" {
				n++
			}
		}
		return n
	}
	cur, _ := before["cur"].([]any)
	for _, r := range cur {
		if m, _ := r.(map[string]any); m["kind"] == "thinking" {
			return count(after) > count(before)
		}
	}
	return true
}

// The random runner rarely gets past a few steps; the generated walks
// reach every state (every transition under MODEL_COVER=transitions),
// sharded over four serves.
func TestStreamPreviewSupersedePaths(t *testing.T) {
	t.Parallel()
	paths := loadPaths(t, "stream_preview_supersede")
	// SPS_PATHS=<k> walks an evenly spaced k of them, to debug.
	if k, err := strconv.Atoi(os.Getenv("SPS_PATHS")); err == nil && k > 0 && k < len(paths) {
		var pick []genPath
		for i := range k {
			pick = append(pick, paths[i*len(paths)/k])
		}
		paths = pick
	}
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newStreamPreviewSupersedeAdapter(t)
			for i := sh; i < len(paths); i += shards {
				if err := a.walk(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			g, err := tracecheck.Load(sppTestdata())
			if err != nil {
				t.Fatal(err)
			}
			for _, id := range a.ids {
				entries := sessionHistory(t, a.s.Home, id)
				if v := g.Check(streamPreviewSupersedeHistory(entries)); v != nil {
					var kinds []string
					for _, e := range entries {
						s, _ := e.Data["text"].(string)
						kinds = append(kinds, fmt.Sprintf("%s(%s)", e.Kind, strings.TrimSpace(s)))
					}
					t.Errorf("walk %v: history trace is not a path in the model: %v\nhistory: %v", a.walks[id], v, kinds)
				}
			}
		})
	}
}

// The path walk proves nothing unless a wrong page fails it. The bug
// shows only where a record lands while a catch-up is in flight, so the
// walks here take every transition whatever MODEL_COVER says.
func TestStreamPreviewSupersedePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	b, err := pathsJSONCover("stream_preview_supersede", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	a := newStreamPreviewSupersedeAdapter(t)
	a.dropAtLanding = true
	for _, p := range doc.Paths {
		if err := a.walk(p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with a catch-up that drops by the landing-time mark; the walk is not checking state")
}

func sppTestdata() string {
	return strings.TrimSuffix(specPath("x"), "specs/x.fizz") + "testdata/stream_preview_supersede"
}

// streamPreviewSupersedeHistory reads the engine's half of a walk off its
// transcript; the steps check disk and turn. Fragments show only where an
// entry holds them, so each is streamed right before the step that
// records it, and one no entry holds (thinking with no summary or cut by
// Stop, or text a retry dropped) is streamed as thinking where the next
// step needs it, then reset when a response records what came after it.
// That is one path the transcript allows, not necessarily the one taken.
func streamPreviewSupersedeHistory(entries []history.Entry) []tracecheck.Step {
	var disk []any
	turn := "running"
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Preview#0.disk": []any{}, "Preview#0.turn": turn}}}
	do := func(name string) {
		steps = append(steps, tracecheck.Step{Action: "Preview#0." + name,
			State: map[string]any{"Preview#0.disk": append([]any{}, disk...), "Preview#0.turn": turn}})
	}
	// The walk starts after the prompt's input: the held request.
	i := slices.IndexFunc(entries, func(e history.Entry) bool { return e.Kind == "input" })
	if i < 0 {
		return steps
	}
	entries = entries[i+1:]
	text := func(e history.Entry) string { s, _ := e.Data["text"].(string); return s }
	holds := func(e history.Entry) []int {
		if e.Kind == "thinking" || e.Kind == "assistant" {
			return sppFrags(text(e))
		}
		return nil
	}
	n, pending := 0, 0 // fragments streamed; of them, the request's in flight
	queued := false
	delta := func(kind string) {
		n++
		pending++
		if kind == "assistant" {
			do("TextDelta")
		} else {
			do("ThinkingDelta")
		}
	}
	// gaps streams the fragments below f that no entry holds.
	gaps := func(f int) {
		for n < f-1 {
			delta("thinking")
		}
	}
	reset := func() {
		if pending > 0 {
			do("DeltaReset")
			pending = 0
		}
	}
	var partial []any
	for j := 0; j < len(entries); j++ {
		e := entries[j]
		switch {
		case e.Kind == "input" && e.Data["steer"] == true:
			// A steer comes while fragment 1 streams.
			if pending == 0 {
				kind := "thinking"
				for _, x := range entries[j+1:] {
					if fs := holds(x); len(fs) > 0 {
						if fs[0] == 1 {
							kind = x.Kind
						}
						break
					}
				}
				delta(kind)
			}
			disk = append(disk, sppEntry(e.Kind, text(e), e.Data))
			queued = true
			do("Steer")
		case e.Kind == "assistant" && e.Data["partial"] == true:
			if fs := holds(e); len(fs) > 0 {
				gaps(fs[0])
				for _, f := range fs {
					if f > n {
						delta("assistant")
					}
				}
			}
			partial = append(partial, sppEntry(e.Kind, text(e), e.Data))
		case len(holds(e)) > 0 || e.Kind == "call":
			// One response: its records share a harness seq, then the
			// call it goes on with.
			k := j
			for k < len(entries) && len(holds(entries[k])) > 0 && entries[k].Data["partial"] != true &&
				reflect.DeepEqual(entries[k].Data["hseq"], e.Data["hseq"]) {
				k++
			}
			lo := 0 // the lowest fragment the response holds
			for _, g := range entries[j:k] {
				if fs := holds(g); len(fs) > 0 {
					lo = fs[0]
					break
				}
			}
			// The response a queued steer asked for goes on without a
			// call. A call right after records is theirs unless they hold
			// the fragment that streamed when the steer came: then they
			// are that response's and the call the next one's. Otherwise
			// the steer's response came first and recorded nothing
			// (thinking with no summary).
			if k < len(entries) && entries[k].Kind == "call" && !(queued && k > j && lo <= n) {
				if queued {
					if pending == 0 {
						delta("thinking")
					}
					do("Respond")
					queued, pending = false, 0
				}
				k++
			}
			group := entries[j:k]
			if lo == 0 {
				if pending == 0 {
					delta("thinking")
				}
			} else {
				if lo > n {
					// What streamed before is not in this response: a
					// retry dropped it.
					gaps(lo)
					reset()
				}
				for _, g := range group {
					for _, f := range holds(g) {
						if f > n {
							delta(g.Kind)
						}
					}
				}
			}
			for _, g := range group {
				if r := sppEntry(g.Kind, text(g), g.Data); r != nil {
					disk = append(disk, r)
				}
			}
			do("Respond")
			queued, pending = false, 0
			j = k - 1
		case e.Kind == "done":
			stopped := slices.ContainsFunc(entries[:j], func(x history.Entry) bool { return x.Kind == "cancelled" })
			if stopped && pending == 0 {
				delta("thinking")
			}
			if !stopped {
				reset()
			}
			disk = append(append(disk, partial...), sppEntry(e.Kind, text(e), e.Data))
			turn = "done"
			if stopped {
				do("Stop")
			} else {
				do("Finish")
			}
			return steps
		}
	}
	return steps
}

func init() { historyProjections["stream_preview_supersede"] = streamPreviewSupersedeHistory }
