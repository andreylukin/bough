//go:build !windows

package mbt

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
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

// specs/live_transcript_sync.fizz against a real serve. The server's
// half is real: session #0's child records and streams through the
// supervisor, the adapter reads the raw SSE stream (id lines included)
// and the ring by connecting, and every transcript read is a real GET.
// The page's half (lines, the cursor, the catch-up timer, the initial
// GET's slot) is the adapter itself, doing what the `selected` effect
// in web/src/app.tsx does, one model step at a time.
//
// The abstraction, spec value <- what the adapter sees:
//   - session #0 is a fresh session per walk whose first turn is held
//     in llm-control. Everything it wrote before the walk (meta, input,
//     the "model is thinking" activity) is outside the model: history
//     seqs up to base0, supervisor seqs up to baseEv0.
//   - Record releases that turn; its recorded frames (assistant, done,
//     usage) are the spec's one "rec" frame and its entries the spec's
//     one entry, seq 1. The reply's own deltas are flushed before the
//     first record, inside the same step; the spec folds them into it.
//   - Ephemeral is a say (control.Say) out of the held turn: one Seq-0
//     assistant-delta, never recorded.
//   - session #1 is one finished session per serve, its whole
//     transcript the spec's STATIC_DISK entry.
//   - `activity` frames are recorded but the page takes them as a
//     label, never as something to catch up on; they are not frames here.
//   - the subscriber's channel holds 256 in the product and CAP=1 in the
//     spec. The adapter cannot make the server fill 256 in one step, so
//     where the spec's fan-out finds the channel full, the adapter ends
//     the stream itself, before the frame is sent — the spec's own
//     reading: to the page a dropped subscriber and a dropped stream are
//     one thing. What happens next (the frames already read still
//     delivered, the EventSource reconnecting, the ring coming again) is
//     the server's.
//   - CatchUpFail is the network failing the request: no request is made.

type ltsFrame struct {
	id *int64 // the SSE id line; nil when the frame had none
	ev serve.Event
}

// ltsStream is one EventSource: the raw frames, id lines kept, which
// servetest.Stream drops.
type ltsStream struct {
	cancel context.CancelFunc
	frames chan ltsFrame // closed when the response ends
}

func ltsOpen(s *servetest.Server, id string) (*ltsStream, error) {
	ctx, cancel := context.WithCancel(context.Background())
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, s.URL+"/api/sessions/"+url.PathEscape(id)+"/events", nil)
	if err != nil {
		cancel()
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		cancel()
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		resp.Body.Close()
		cancel()
		return nil, fmt.Errorf("events %s: %s", id, resp.Status)
	}
	st := &ltsStream{cancel: cancel, frames: make(chan ltsFrame, 4096)}
	go func() {
		defer close(st.frames)
		defer resp.Body.Close()
		sc := bufio.NewScanner(resp.Body)
		sc.Buffer(make([]byte, 0, 64<<10), 16<<20)
		var f ltsFrame
		var data strings.Builder
		for sc.Scan() {
			line := sc.Text()
			switch {
			case line == "":
				if data.Len() > 0 && json.Unmarshal([]byte(data.String()), &f.ev) == nil {
					st.frames <- f
				}
				f, data = ltsFrame{}, strings.Builder{}
			case strings.HasPrefix(line, "id: "):
				if n, err := strconv.ParseInt(strings.TrimPrefix(line, "id: "), 10, 64); err == nil {
					f.id = &n
				}
			case strings.HasPrefix(line, "data: "):
				data.WriteString(strings.TrimPrefix(line, "data: "))
			}
		}
	}()
	return st, nil
}

func (st *ltsStream) close() {
	if st != nil {
		st.cancel()
	}
}

// ltsQuiet is how long a stream must stay silent for a burst (the ring's
// replay, a turn's trailing usage) to count as over.
const ltsQuiet = 200 * time.Millisecond

// drain reads frames until the stream is quiet for ltsQuiet. ended says
// the response ended.
func (st *ltsStream) drain() (fs []ltsFrame, ended bool) {
	for {
		select {
		case f, ok := <-st.frames:
			if !ok {
				return fs, true
			}
			fs = append(fs, f)
		case <-time.After(ltsQuiet):
			return fs, false
		}
	}
}

// until reads frames until one satisfies ok, then drains the rest of
// the burst.
func (st *ltsStream) until(what string, ok func(ltsFrame) bool) ([]ltsFrame, error) {
	var fs []ltsFrame
	deadline := time.After(actionTimeout)
	for {
		select {
		case f, open := <-st.frames:
			if !open {
				return fs, fmt.Errorf("stream ended waiting for %s", what)
			}
			fs = append(fs, f)
			if ok(f) {
				more, _ := st.drain()
				return append(fs, more...), nil
			}
		case <-deadline:
			return fs, fmt.Errorf("no %s on the stream after %s", what, actionTimeout)
		}
	}
}

// ltsLine is a line on the page: the session whose GET brought it.
type ltsLine struct {
	sess int
	line serve.Line
}

type ltsGet struct {
	sess    int
	served  bool
	entries []serve.Line
}

type ltsFetch struct {
	sess  int
	since int64 // the real cursor, a history seq
	stale bool  // sent by a selection since left
}

type liveTranscriptSyncAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	// session #1: finished once per serve; its transcript is (0, last1].
	static string
	last1  int64
	seqs1  map[int64]bool

	// session #0, this walk's.
	live    string
	turn    int
	held    string
	says    int
	base0   int64          // last history seq before the walk
	baseEv0 int64          // last supervisor seq before the walk
	seqs0   map[int64]bool // the recorded turn's history seqs, once recorded
	last0   int64
	disk    int
	ring    []map[string]any
	ids     []string

	// the page
	sel    int
	conn   string
	stream *ltsStream
	inbox  []map[string]any
	lines  []ltsLine
	loaded bool
	get    *ltsGet
	fetch  *ltsFetch
	timer  bool
	paused bool

	// replayArmsNothing is the deliberate bug the wrong-adapter tests
	// inject: the page ignores what the ring replays on (re)connect.
	replayArmsNothing bool
}

func newLiveTranscriptSyncAdapter(t *testing.T) *liveTranscriptSyncAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &liveTranscriptSyncAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	// The finished session, once: nothing in the model writes to it.
	control.Queue(t, a.dir, "a0000", control.Turn{Mode: "ok", Text: "static reply"})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "static"), "static turn")
	if err != nil {
		t.Fatal(err)
	}
	a.static = row.ID
	if _, err := waitRow(s, row.ID, "the static turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	lines, err := a.settled(row.ID)
	if err != nil {
		t.Fatal(err)
	}
	a.seqs1 = map[int64]bool{}
	for _, l := range lines {
		a.seqs1[l.Seq] = true
		a.last1 = max(a.last1, l.Seq)
	}
	return a
}

// settled reads a transcript until two reads a quiet apart agree: a
// turn's summary and title land after its done.
func (a *liveTranscriptSyncAdapter) settled(id string) ([]serve.Line, error) {
	var prev []serve.Line
	for range 100 {
		lines, err := a.read(id, 0)
		if err != nil {
			return nil, err
		}
		if prev != nil && len(lines) == len(prev) {
			return lines, nil
		}
		prev = lines
		time.Sleep(ltsQuiet)
	}
	return nil, fmt.Errorf("transcript of %s never settled", id)
}

// read is GET /api/sessions/{id}?since=: the page's api.session.
func (a *liveTranscriptSyncAdapter) read(id string, since int64) ([]serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("%s/api/sessions/%s?since=%d", a.s.URL, url.PathEscape(id), since), nil)
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
		return nil, fmt.Errorf("GET %s: %s", id, resp.Status)
	}
	var r struct {
		Entries []serve.Line `json:"entries"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return nil, err
	}
	return r.Entries, nil
}

func (a *liveTranscriptSyncAdapter) sessID(sess int) string {
	if sess == 1 {
		return a.static
	}
	return a.live
}

// Init starts a walk on a fresh session #0 whose turn is held, and a
// page that has selected nothing.
//
// The session is made at the walk's first step that is taken, not here:
// most walks the runner draws stop at a disabled first action, and the
// state Init reports (nothing on disk past the baseline, an empty ring,
// a page with nothing selected) is as true of a session not made yet.
func (a *liveTranscriptSyncAdapter) Init() error {
	a.stream.close()
	*a = liveTranscriptSyncAdapter{t: a.t, s: a.s, dir: a.dir, static: a.static, last1: a.last1, seqs1: a.seqs1,
		turn: a.turn, ids: a.ids, replayArmsNothing: a.replayArmsNothing}
	a.sel, a.conn = -1, "none"
	a.gate.reset()
	return nil
}

// step is each action's preamble: frames that came between steps, the
// spec's `require` through the gate, and session #0 on first use.
func (a *liveTranscriptSyncAdapter) step(enabled bool) (bool, error) {
	a.pump()
	if !a.gate.pass(enabled) {
		return false, nil
	}
	if a.live == "" {
		if err := a.prepare(); err != nil {
			return false, err
		}
	}
	return true, nil
}

// prepare makes session #0 with its turn held and reads the baseline.
func (a *liveTranscriptSyncAdapter) prepare() error {
	a.turn++
	name := fmt.Sprintf("w%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "recorded " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.live, a.held = row.ID, name
	a.ids = append(a.ids, row.ID)
	if err := a.s.Prompt(ctx, row.ID, "turn "+name); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	lines, err := a.settled(row.ID)
	if err != nil {
		return err
	}
	for _, l := range lines {
		a.base0 = max(a.base0, l.Seq)
	}
	a.last0 = a.base0
	ring, err := a.probe()
	if err != nil {
		return err
	}
	for _, f := range ring {
		a.baseEv0 = max(a.baseEv0, f.ev.Seq)
	}
	for _, f := range ring {
		if x := a.abs(0, f); x != nil {
			a.ring = append(a.ring, x) // only a delta gets here; RingHoldsRecordsOnly shows it
		}
	}
	return nil
}

// probe reads session #0's ring the way a new subscriber gets it.
func (a *liveTranscriptSyncAdapter) probe() ([]ltsFrame, error) {
	st, err := ltsOpen(a.s, a.live)
	if err != nil {
		return nil, err
	}
	defer st.close()
	fs, _ := st.drain()
	return fs, nil
}

// abs is what one real frame is in the spec: nil when it is outside the
// model (before the walk, or an activity label).
func (a *liveTranscriptSyncAdapter) abs(sess int, f ltsFrame) map[string]any {
	id := 0 // no SSE id line
	if f.id != nil {
		id = int(*f.id)
	}
	if f.ev.Seq == 0 {
		if f.id != nil {
			id = -1 // any id line, "id: 0" included, is one too many
		}
		return map[string]any{"kind": "delta", "seq": 0, "id": id}
	}
	if f.ev.Kind == "activity" || (sess == 0 && f.ev.Seq <= a.baseEv0) {
		return nil
	}
	// The spec's frame carries the entry it announces (seq 1); its id
	// is that seq exactly when the SSE id is the frame's own seq.
	if f.id == nil || *f.id != f.ev.Seq {
		id = -1
	} else {
		id = 1
	}
	return map[string]any{"kind": "rec", "seq": 1, "id": id}
}

// replay reads what a fresh subscription replays and returns whether
// it arms the page's catch-up, as the spec's replay() does. A delta in
// the replay goes into the reported ring, where RingHoldsRecordsOnly
// makes it a mismatch.
func (a *liveTranscriptSyncAdapter) replay(sess int) error {
	a.stream.close()
	st, err := ltsOpen(a.s, a.sessID(sess))
	if err != nil {
		return err
	}
	a.stream = st
	fs, ended := st.drain()
	if ended {
		return errors.New("the event stream ended during its replay")
	}
	for _, f := range fs {
		switch x := a.abs(sess, f); {
		case x == nil:
		case x["kind"] == "delta":
			a.ring = append(a.ring, x)
		case !a.replayArmsNothing:
			a.timer = true
		}
	}
	return nil
}

// pump takes frames the stream brought outside any step: none are
// expected, so each one lands in the inbox, where the state shows it.
func (a *liveTranscriptSyncAdapter) pump() {
	if a.stream == nil || a.conn != "live" {
		return
	}
	for {
		select {
		case f, ok := <-a.stream.frames:
			if !ok {
				a.conn = "dropped" // the server ended it: the state says so
				return
			}
			if x := a.abs(a.sel, f); x != nil {
				a.inbox = append(a.inbox, x)
			}
		default:
			return
		}
	}
}

// fanning is the spec's fanout() before the frame is sent: whether the
// page's stream will take this step's frame. A full channel drops the
// subscriber first (see the abstraction above).
func (a *liveTranscriptSyncAdapter) fanning() bool {
	if a.sel != 0 || a.conn != "live" {
		return false
	}
	if len(a.inbox) >= 1 { // CAP
		a.stream.close()
		a.stream = nil
		a.conn = "dropped"
		return false
	}
	return true
}

func (a *liveTranscriptSyncAdapter) Cleanup() error {
	a.stream.close()
	a.stream = nil
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.live, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *liveTranscriptSyncAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Sync", Index: 0}: a}, nil
}

// count is the spec's number of entries a set of real lines of sess
// holds: 0 none, 1 the whole of the modelled entry, -1 anything else
// (part of it, or entries the model has no place for).
func (a *liveTranscriptSyncAdapter) count(sess int, lines []serve.Line) int {
	lo, seqs := a.base0, a.seqs0
	if sess == 1 {
		lo, seqs = 0, a.seqs1
	}
	n := 0
	for _, l := range lines {
		if l.Seq <= lo {
			continue
		}
		if !seqs[l.Seq] {
			return -1
		}
		n++
	}
	switch {
	case n == 0:
		return 0
	case n == len(seqs):
		return 1
	}
	return -1
}

// cursor is the spec's last_seq for a real cursor.
func (a *liveTranscriptSyncAdapter) cursor(sess int, c int64) int {
	lo, hi := a.base0, a.last0
	if sess == 1 {
		lo, hi = 0, a.last1
	}
	switch {
	case c <= lo:
		return 0
	case c == hi:
		return 1
	}
	return -1
}

func (a *liveTranscriptSyncAdapter) GetState() (map[string]any, error) {
	a.pump()
	lines := []any{}
	if len(a.lines) > 0 {
		sess := a.lines[0].sess
		var ls []serve.Line
		seen := map[int64]bool{}
		dup := false
		for _, l := range a.lines {
			if l.sess != sess {
				// Two sessions' lines at once: say so in the one way
				// the spec can see it.
				lines = append(lines, map[string]any{"sess": l.sess, "seq": -1})
			}
			if seen[l.line.Seq] {
				dup = true
			}
			seen[l.line.Seq] = true
			ls = append(ls, l.line)
		}
		if n := a.count(sess, ls); n != 0 {
			lines = append(lines, map[string]any{"sess": sess, "seq": n})
		}
		if dup {
			lines = append(lines, map[string]any{"sess": sess, "seq": 1})
		}
	}
	// The spec spells an empty request slot sess -1 (see its header).
	get := map[string]any{"sess": -1, "snap": -1}
	if a.get != nil {
		snap := -1
		if a.get.served {
			snap = a.count(a.get.sess, a.get.entries)
		}
		get = map[string]any{"sess": a.get.sess, "snap": snap}
	}
	fetch := map[string]any{"sess": -1, "since": 0, "live": false}
	if a.fetch != nil {
		fetch = map[string]any{"sess": a.fetch.sess, "since": a.cursor(a.fetch.sess, a.fetch.since), "live": !a.fetch.stale}
	}
	return map[string]any{
		"disk": a.disk, "ring": list(a.ring), "sel": a.sel, "conn": a.conn,
		"inbox": list(a.inbox), "lines": lines, "loaded": a.loaded,
		"get": get, "fetch": fetch, "timer": a.timer, "paused": a.paused,
	}, nil
}

// list is frames as a spec list: never nil, which the wire would not
// send as [].
func list(ms []map[string]any) []any {
	out := []any{}
	for _, m := range ms {
		out = append(out, m)
	}
	return out
}

// --- server: session #0's child ---

func (a *liveTranscriptSyncAdapter) Ephemeral() error {
	if ok, err := a.step(a.disk < 1); !ok {
		return err
	}
	sub := a.fanning()
	a.says++
	control.Say(a.t, a.dir, a.held, a.says, fmt.Sprintf("say %d ", a.says))
	deadline := time.Now().Add(actionTimeout)
	for !control.Said(a.dir, a.held, a.says) {
		if time.Now().After(deadline) {
			return fmt.Errorf("say %d never streamed", a.says)
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !sub {
		time.Sleep(ltsQuiet) // past the 50ms delta buffer: the fan-out is over
		return nil
	}
	fs, err := a.stream.until("the say's delta", func(f ltsFrame) bool { return f.ev.Seq == 0 })
	if err != nil {
		return err
	}
	// The step's deltas are the spec's one delta frame; anything else
	// it brought (a delta with an id line, a record) is shown beside it.
	a.inbox = append(a.inbox, map[string]any{"kind": "delta", "seq": 0, "id": 0})
	for _, f := range fs {
		if x := a.abs(0, f); x != nil && (x["kind"] != "delta" || x["id"] != 0) {
			a.inbox = append(a.inbox, x)
		}
	}
	return nil
}

func (a *liveTranscriptSyncAdapter) Record() error {
	if ok, err := a.step(a.disk < 1); !ok {
		return err
	}
	sub := a.fanning()
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if _, err := waitRow(a.s, a.live, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	lines, err := a.settled(a.live)
	if err != nil {
		return err
	}
	a.seqs0 = map[int64]bool{}
	for _, l := range lines {
		if l.Seq > a.base0 {
			a.seqs0[l.Seq] = true
			a.last0 = max(a.last0, l.Seq)
		}
	}
	a.disk = 0
	for _, l := range lines {
		if l.Seq > a.base0 && l.Kind == "done" {
			a.disk++
		}
	}
	if sub {
		fs, err := a.stream.until("the turn's done", func(f ltsFrame) bool { return f.ev.Kind == "done" })
		if err != nil {
			return err
		}
		// The recorded frames are the spec's one rec frame; the
		// reply's deltas ride in it, but must still have no id line.
		var frame map[string]any
		for _, f := range fs {
			x := a.abs(0, f)
			switch {
			case x == nil:
			case x["kind"] == "delta":
				if x["id"] != 0 {
					a.inbox = append(a.inbox, x)
				}
			case frame == nil:
				frame = x
			case x["id"] != frame["id"]:
				frame = x // a rec frame with a wrong id is the one to show
			}
		}
		if frame != nil {
			a.inbox = append(a.inbox, frame)
		}
	}
	ring, err := a.probe()
	if err != nil {
		return err
	}
	a.ring = nil
	var last map[string]any
	for _, f := range ring {
		switch x := a.abs(0, f); {
		case x == nil:
		case x["kind"] == "delta":
			a.ring = append(a.ring, x)
		default:
			last = x
		}
	}
	if last != nil {
		a.ring = append(a.ring, last) // RING = 1: the newest record
	}
	return nil
}

// --- the page ---

func (a *liveTranscriptSyncAdapter) Open() error {
	if ok, err := a.step(a.sel == -1); !ok {
		return err
	}
	a.sel, a.conn = 0, "live"
	if err := a.replay(0); err != nil {
		return err
	}
	a.get = &ltsGet{sess: 0}
	return nil
}

func (a *liveTranscriptSyncAdapter) Switch() error {
	if ok, err := a.step(a.sel >= 0); !ok {
		return err
	}
	a.sel = 1 - a.sel
	a.lines, a.loaded, a.timer, a.paused = nil, false, false, false
	a.conn, a.inbox = "live", nil
	if a.fetch != nil {
		a.fetch.stale = true // the old run's: its answer is ignored whenever it lands
	}
	if err := a.replay(a.sel); err != nil {
		return err
	}
	a.get = &ltsGet{sess: a.sel}
	return nil
}

func (a *liveTranscriptSyncAdapter) InitialServe() error {
	if ok, err := a.step(a.get != nil && !a.get.served); !ok {
		return err
	}
	entries, err := a.read(a.sessID(a.get.sess), 0)
	if err != nil {
		return err
	}
	a.get.served, a.get.entries = true, entries
	return nil
}

func (a *liveTranscriptSyncAdapter) InitialOk() error {
	if ok, err := a.step(a.get != nil && a.get.served); !ok {
		return err
	}
	// setLines(r.entries): a replace, as app.tsx does it.
	a.lines = nil
	for _, l := range a.get.entries {
		a.lines = append(a.lines, ltsLine{sess: a.get.sess, line: l})
	}
	a.get, a.loaded = nil, true
	return nil
}

func (a *liveTranscriptSyncAdapter) Deliver() error {
	if ok, err := a.step(len(a.inbox) > 0); !ok {
		return err
	}
	f := a.inbox[0]
	a.inbox = a.inbox[1:]
	if f["kind"] == "rec" {
		a.timer = true
	}
	return nil
}

func (a *liveTranscriptSyncAdapter) Reconnect() error {
	if ok, err := a.step(a.conn == "dropped" && len(a.inbox) == 0); !ok {
		return err
	}
	a.conn = "live"
	return a.replay(a.sel)
}

func (a *liveTranscriptSyncAdapter) lastSeq() int64 {
	if len(a.lines) == 0 {
		return 0
	}
	return a.lines[len(a.lines)-1].line.Seq
}

func (a *liveTranscriptSyncAdapter) CatchUpFire() error {
	if ok, err := a.step(a.timer && a.fetch == nil); !ok {
		return err
	}
	a.timer = false
	a.fetch = &ltsFetch{sess: a.sel, since: a.lastSeq()}
	return nil
}

func (a *liveTranscriptSyncAdapter) Retry() error {
	if ok, err := a.step(a.paused && a.fetch == nil); !ok {
		return err
	}
	a.timer = false
	a.fetch = &ltsFetch{sess: a.sel, since: a.lastSeq()}
	return nil
}

func (a *liveTranscriptSyncAdapter) CatchUpOk() error {
	if ok, err := a.step(a.fetch != nil); !ok {
		return err
	}
	f := a.fetch
	a.fetch = nil
	if f.stale {
		return nil // `live` is false: the response is ignored
	}
	a.paused = false
	entries, err := a.read(a.sessID(f.sess), f.since)
	if err != nil {
		return err
	}
	seen := map[int64]bool{}
	for _, l := range a.lines {
		seen[l.line.Seq] = true
	}
	for _, e := range entries {
		if !seen[e.Seq] {
			a.lines = append(a.lines, ltsLine{sess: f.sess, line: e})
		}
	}
	return nil
}

func (a *liveTranscriptSyncAdapter) CatchUpFail() error {
	if ok, err := a.step(a.fetch != nil); !ok {
		return err
	}
	f := a.fetch
	a.fetch = nil
	if !f.stale {
		a.paused, a.timer = true, true
	}
	return nil
}

var liveTranscriptSyncActions = map[string]map[string]fmbt.ActionFunc{"Sync": {
	"Ephemeral":    action((*liveTranscriptSyncAdapter).Ephemeral),
	"Record":       action((*liveTranscriptSyncAdapter).Record),
	"Open":         action((*liveTranscriptSyncAdapter).Open),
	"Switch":       action((*liveTranscriptSyncAdapter).Switch),
	"InitialServe": action((*liveTranscriptSyncAdapter).InitialServe),
	"InitialOk":    action((*liveTranscriptSyncAdapter).InitialOk),
	"Deliver":      action((*liveTranscriptSyncAdapter).Deliver),
	"Reconnect":    action((*liveTranscriptSyncAdapter).Reconnect),
	"CatchUpFire":  action((*liveTranscriptSyncAdapter).CatchUpFire),
	"Retry":        action((*liveTranscriptSyncAdapter).Retry),
	"CatchUpOk":    action((*liveTranscriptSyncAdapter).CatchUpOk),
	"CatchUpFail":  action((*liveTranscriptSyncAdapter).CatchUpFail),
}}

// The runner picks each action at random among all twelve, and a walk
// ends at its first disabled one, so most walks are one or two steps and
// cost nothing (the session is made lazily). The wrong adapter needs a
// Switch, or an Open after a Record, to show: about one walk in seventy.
// 600 walks miss it with odds under 1e-3. LTS_SEQ_RUNS overrides.
func liveTranscriptSyncOptions() map[string]any {
	runs := 600
	if n, err := strconv.Atoi(os.Getenv("LTS_SEQ_RUNS")); err == nil && n > 0 {
		runs = n
	}
	return map[string]any{"max-seq-runs": runs, "max-actions": 10, "max-parallel-runs": 0}
}

// liveTranscriptSyncHistory reads session #0's transcript: the one thing
// its history shows of the model is Record, the held turn's done.
func liveTranscriptSyncHistory(entries []history.Entry) []tracecheck.Step {
	disk := func(n int) map[string]any { return map[string]any{"Sync#0.disk": n} }
	steps := []tracecheck.Step{{Action: "Init", State: disk(0)}}
	n := 0
	for _, e := range entries {
		if e.Kind == "done" {
			n++
			steps = append(steps, tracecheck.Step{Action: "Sync#0.Record", State: disk(n)})
		}
	}
	return steps
}

func init() { historyProjections["live_transcript_sync"] = liveTranscriptSyncHistory }

func TestLiveTranscriptSync(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newLiveTranscriptSyncAdapter(t)
	if err := runMBT(t, "live_transcript_sync", a, liveTranscriptSyncActions, liveTranscriptSyncOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "live_transcript_sync"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), liveTranscriptSyncHistory)
	}
}

// ltsPath is one generated path (testdata/<spec>/paths.json).
type ltsPath struct {
	Trace []struct {
		Action string         `json:"action"`
		State  map[string]any `json:"state"`
	} `json:"trace"`
}

// walk drives one generated path through the adapter and compares the
// Sync role's state after every step. Every action on a path is enabled
// in the spec, so the gate closing is itself a mismatch.
func (a *liveTranscriptSyncAdapter) walk(p ltsPath) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for i, st := range p.Trace {
		name := strings.TrimPrefix(st.Action, "Sync#0.")
		if st.Action == "Init" {
			err = a.Init()
		} else if f, ok := liveTranscriptSyncActions["Sync"][name]; ok {
			_, err = f(a, nil)
		} else {
			err = fmt.Errorf("no action %q", st.Action)
		}
		if err == nil && a.gate.off {
			err = errors.New("the adapter found the action disabled")
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		gj, _ := json.Marshal(got)
		var g map[string]any
		json.Unmarshal(gj, &g)
		for k, want := range st.State {
			field, ok := strings.CutPrefix(k, "Sync#0.")
			if !ok {
				continue // the ghost and the role reference are not role state
			}
			wj, _ := json.Marshal(want)
			hj, _ := json.Marshal(g[field])
			if string(wj) != string(hj) {
				return fmt.Errorf("step %d (%s): %s: want %s, got %s", i, name, field, wj, hj)
			}
		}
	}
	return nil
}

// The runner above draws walks at random and stops checking a walk at
// its first disabled action, so it rarely goes deeper than a few steps.
// This walks the generator's paths instead (every transition is on
// one), the ones the browser spec walks. LTS_PATHS=all walks all of
// them; the default is an evenly spaced sample.
func TestLiveTranscriptSyncPaths(t *testing.T) {
	t.Parallel()
	paths := ltsPaths(t)
	n := 24
	if v := os.Getenv("LTS_PATHS"); v == "all" {
		n = len(paths)
	} else if k, err := strconv.Atoi(v); err == nil && k > 0 {
		n = min(k, len(paths))
	}
	var pick []int
	for i := range n {
		pick = append(pick, i*len(paths)/n)
	}
	const workers = 4
	for w := range workers {
		t.Run(fmt.Sprintf("serve%d", w), func(t *testing.T) {
			t.Parallel()
			a := newLiveTranscriptSyncAdapter(t)
			for j := w; j < len(pick); j += workers {
				if err := a.walk(paths[pick[j]]); err != nil {
					t.Errorf("path %d: %v", pick[j], err)
				}
			}
			// Every transcript the walks wrote is a path in the model.
			g, err := tracecheck.Load(ltsTestdata())
			if err != nil {
				t.Fatal(err)
			}
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), liveTranscriptSyncHistory)
			}
		})
	}
}

// The path walk proves nothing unless a wrong page fails it.
func TestLiveTranscriptSyncPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	paths := ltsPaths(t)
	a := newLiveTranscriptSyncAdapter(t)
	a.replayArmsNothing = true
	for i := 0; i < len(paths); i += len(paths) / 8 {
		if a.walk(paths[i]) != nil {
			return
		}
	}
	t.Fatal("eight paths passed with a page that ignores the ring's replay; the walk is not checking state")
}

func ltsTestdata() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "live_transcript_sync")
}

func ltsPaths(t *testing.T) []ltsPath {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(ltsTestdata(), "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []ltsPath `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	return doc.Paths
}

// A page that ignores the ring's replay must fail the run.
func TestLiveTranscriptSyncCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newLiveTranscriptSyncAdapter(t)
	a.replayArmsNothing = true
	if err := runMBT(t, "live_transcript_sync", a, liveTranscriptSyncActions, liveTranscriptSyncOptions()); err == nil {
		t.Fatal("a run whose page ignores the ring's replay passed; the runner is not checking state")
	}
}
