//go:build !windows

package mbt

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/event_stream_lifecycle.fizz against a real serve that the
// adapter stops with SIGTERM and starts again on one HOME and port. The
// server's half is real: the session's child records through
// llm-control, the page's EventSource is a real /events stream, every
// transcript and list read is a real GET, and serve B is a new process
// with a new supervisor. The page's half (its lines, the catch-up timer,
// "Updates paused", the row it shows, what its EventSource does on an
// end or a non-200) is the adapter, doing what api.ts subscribe and the
// `selected` effect in web/src/app.tsx do, one model step at a time.
//
// The abstraction, spec value <- what the adapter sees:
//   - one fresh session per walk, its first turn held in llm-control.
//   - disk and lines count entries the same way over the history file
//     and over the page's lines (which are those entries): a tool call
//     inside an open turn, and a done that closes one. Record releases
//     the held request as one bash call and holds the next. An input is
//     not an entry of its own: the child never prints "input", so a bare
//     prompt fans out nothing but the "model is thinking" label, which
//     arms nothing. Init and NextPrompt are therefore a prompt and its
//     turn's first tool call, the first frame a real turn fans out that
//     the page acts on. A cancelled that closes a dangling turn (the `-r`
//     resume on serve B), and the done after it, are bookkeeping.
//   - the page talks to serve through a reverse proxy, as it does behind
//     `tailscale serve` or --host. While serve is down the proxy answers
//     502, which is the spec's EventsNon200; with serve up it is made to
//     answer one 502 (the network is not modelled any further).
//   - draining: Shutdown and Close run back to back inside serve, too
//     close to put a step between. So SigTerm is the page's half of it:
//     the proxy loses serve (its stream ends, every request is a 502)
//     while serve A and its child run on, and the child's tail (Record,
//     Finish) is real, into A's ring and the history file. CloseKills is
//     the real SIGTERM: serve exits, and a stream held open on serve A
//     since SigTerm (the witness) must have ended by then, which is the
//     spec's StreamsEndWithServe on the server's side.
//   - ring is a new subscriber's replay, read by subscribing: whether it
//     holds a frame the page arms on. While serve is down it is what A's
//     ring was, the last read.
//   - poll is "slow" whenever a thread is selected, which it always is.

// eslProxy is the reverse proxy in front of serve.
type eslProxy struct {
	url    string
	glitch atomic.Bool // answer the next request 502
	down   atomic.Bool // serve is gone: answer every request 502
}

func newESLProxy(t *testing.T, target string) *eslProxy {
	u, err := url.Parse(target)
	if err != nil {
		t.Fatal(err)
	}
	rp := httputil.NewSingleHostReverseProxy(u)
	rp.FlushInterval = -1
	rp.ErrorHandler = func(w http.ResponseWriter, _ *http.Request, _ error) { w.WriteHeader(http.StatusBadGateway) }
	p := &eslProxy{}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	srv := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if p.down.Load() || p.glitch.CompareAndSwap(true, false) {
			w.WriteHeader(http.StatusBadGateway)
			return
		}
		rp.ServeHTTP(w, r)
	})}
	go srv.Serve(ln)
	t.Cleanup(func() { srv.Close() })
	p.url = "http://" + ln.Addr().String()
	return p
}

type eventStreamLifecycleAdapter struct {
	t     *testing.T
	s     *servetest.Server
	proxy *eslProxy
	dir   string
	gate  gate

	live string // this walk's session
	held string // its turn held in llm-control, "" when none
	turn int
	ids  []string

	up      bool   // the serve process runs
	serve   string // a | draining | down | b
	ring    bool   // the last ring read: serve A's while serve is down
	witness *ltsStream

	// the page
	es     string
	stream *ltsStream
	lines  []serve.Line
	timer  bool
	paused bool
	row    string

	// retryResubscribes is TestEventStreamLifecyclePathsCatchWrongAdapter's
	// bug: Retry also opens a new EventSource.
	retryResubscribes bool
}

func newEventStreamLifecycleAdapter(t *testing.T) *eventStreamLifecycleAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &eventStreamLifecycleAdapter{t: t, s: s, dir: control.Dir(s.Home), up: true}
	a.proxy = newESLProxy(t, s.URL)
	t.Cleanup(func() { a.stream.close(); a.witness.close() })
	return a
}

func (a *eventStreamLifecycleAdapter) nextTurn() string {
	a.turn++
	return fmt.Sprintf("e%05d", a.turn)
}

// Init archives the last walk's session (which kills its child), brings
// serve back if the walk left it down, and opens a fresh session with a
// held turn, selected on the page: the whole transcript read, the stream
// open, and the catch-up its replay armed landed.
func (a *eventStreamLifecycleAdapter) Init() error {
	a.gate.reset()
	a.stream.close()
	a.witness.close()
	a.stream, a.witness = nil, nil
	if !a.up {
		if err := a.s.Resume(""); err != nil {
			return err
		}
		a.up = true
	}
	a.proxy.down.Store(false)
	ctx, cancel := actionCtx()
	defer cancel()
	if a.live != "" {
		if _, err := a.s.Archive(ctx, a.live); err != nil {
			return fmt.Errorf("archiving the last walk's session: %w", err)
		}
	}
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "turn "+name)
	if err != nil {
		return err
	}
	a.live, a.held = row.ID, name
	a.ids = append(a.ids, row.ID)
	if err := a.taken(name); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.live, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	if err := a.call(); err != nil {
		return err
	}
	if err := a.waitUnits(1); err != nil {
		return err
	}
	a.serve = "a"
	a.lines, a.timer, a.paused, a.row = nil, false, false, ""
	if err := a.selectThread(); err != nil {
		return err
	}
	if a.timer {
		return a.catchUp()
	}
	return nil
}

// Cleanup has nothing to do: the next Init archives the session, which
// kills its child, and the test's end stops serve.
func (a *eventStreamLifecycleAdapter) Cleanup() error { return nil }

// --- the page's requests ---

func (a *eventStreamLifecycleAdapter) request(ctx context.Context, path string) (*http.Response, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.proxy.url+path, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	return http.DefaultClient.Do(req)
}

// session is the page's api.session(id, since).
func (a *eventStreamLifecycleAdapter) session(since int64) (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	resp, err := a.request(ctx, fmt.Sprintf("/api/sessions/%s?since=%d", url.PathEscape(a.live), since))
	if err != nil {
		return serve.Row{}, nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return serve.Row{}, nil, fmt.Errorf("GET session: %s", resp.Status)
	}
	var r struct {
		Session serve.Row    `json:"session"`
		Entries []serve.Line `json:"entries"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return serve.Row{}, nil, err
	}
	return r.Session, r.Entries, nil
}

// connect is `new EventSource(...)`'s request: the stream on a 200,
// else the status it got (0 for a network error).
func (a *eventStreamLifecycleAdapter) connect() (*ltsStream, int, error) {
	ctx, cancel := context.WithCancel(context.Background())
	resp, err := a.request(ctx, "/api/sessions/"+url.PathEscape(a.live)+"/events")
	if err != nil {
		cancel()
		return nil, 0, err
	}
	if resp.StatusCode != http.StatusOK {
		resp.Body.Close()
		cancel()
		return nil, resp.StatusCode, nil
	}
	return eslFollow(resp, cancel), http.StatusOK, nil
}

// eslFollow reads an open /events response as frames, as ltsOpen does.
func eslFollow(resp *http.Response, cancel context.CancelFunc) *ltsStream {
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
	return st
}

// eslArms is whether a frame arms the page's catch-up: what the
// subscribe callback in app.tsx does not return early on.
func eslArms(f ltsFrame) bool {
	ev := f.ev
	switch ev.Kind {
	case "activity", "call-delta", "assistant-delta", "thinking-delta", "delta-reset":
		return false
	}
	if ev.Kind == "call" || ev.Kind == "sub:call" {
		if ev.Extra["phase"] == "start" {
			return false
		}
	}
	return true
}

// open subscribes (the page's EventSource on a 200) and takes the
// ring's replay: any frame that arms the catch-up sets the timer.
func (a *eventStreamLifecycleAdapter) open() error {
	st, code, err := a.connect()
	if err != nil {
		return fmt.Errorf("event stream: %w", err)
	}
	if code != http.StatusOK {
		return fmt.Errorf("event stream: %d", code)
	}
	a.stream, a.es = st, "open"
	fs, ended := st.drain()
	if ended {
		return errors.New("the event stream ended during its replay")
	}
	for _, f := range fs {
		if eslArms(f) {
			a.timer = true
		}
	}
	return nil
}

// selectThread is the `selected` effect: the whole transcript, a new
// EventSource, the replay; the old stream's timer goes with it.
func (a *eventStreamLifecycleAdapter) selectThread() error {
	a.stream.close()
	a.stream = nil
	row, lines, err := a.session(0)
	if err != nil {
		return err
	}
	a.lines, a.row, a.paused, a.timer = lines, string(row.Status), false, false
	return a.open()
}

// pump takes the frames the stream brought since the last look, and
// notices its end: the EventSource is then CONNECTING.
func (a *eventStreamLifecycleAdapter) pump() {
	if a.stream == nil {
		return
	}
	for {
		select {
		case f, ok := <-a.stream.frames:
			if !ok {
				a.stream, a.es = nil, "connecting"
				return
			}
			if eslArms(f) {
				a.timer = true
			}
		default:
			return
		}
	}
}

// await is a step's own frames: when the stream is open, it waits for
// the first that arms the catch-up (it may be here already), then for
// the burst to end.
func (a *eventStreamLifecycleAdapter) await(what string) error {
	armed := false
	for a.stream != nil && !armed {
		select {
		case f, ok := <-a.stream.frames:
			if !ok {
				a.stream, a.es = nil, "connecting"
				return nil
			}
			armed = eslArms(f)
		default:
			fs, err := a.stream.until(what, eslArms)
			if err != nil {
				return err
			}
			armed = len(fs) > 0
		}
	}
	if !armed {
		return nil
	}
	a.timer = true
	_, ended := a.stream.drain()
	if ended {
		a.stream, a.es = nil, "connecting"
	}
	return nil
}

func (a *eventStreamLifecycleAdapter) lastSeq() int64 {
	if len(a.lines) == 0 {
		return 0
	}
	return a.lines[len(a.lines)-1].Seq
}

// catchUp is the page's catchUp: on success the new lines appended
// (deduped by seq) and the row replaced; on failure "Updates paused"
// and the timer re-armed on its backoff.
func (a *eventStreamLifecycleAdapter) catchUp() error {
	row, lines, err := a.session(a.lastSeq())
	if err != nil {
		a.paused, a.timer = true, true
		return nil
	}
	a.paused, a.timer = false, false
	a.row = string(row.Status)
	seen := map[int64]bool{}
	for _, l := range a.lines {
		seen[l.Seq] = true
	}
	for _, l := range lines {
		if !seen[l.Seq] {
			a.lines = append(a.lines, l)
		}
	}
	return nil
}

// --- reading the state ---

func (a *eventStreamLifecycleAdapter) GetState() (map[string]any, error) {
	a.pump()
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.live+".jsonl"))
	if err != nil {
		return nil, err
	}
	var kinds []string
	for _, e := range entries {
		kinds = append(kinds, e.Kind)
	}
	disk, open := eslUnits(kinds)
	turn := "closed"
	if open {
		turn = "open"
	}
	var lk []string
	for _, l := range a.lines {
		lk = append(lk, l.Kind)
	}
	lines, _ := eslUnits(lk)

	child := false
	if a.up {
		ctx, cancel := actionCtx()
		row, _, err := a.s.GetSession(ctx, a.live)
		cancel()
		if err != nil {
			return nil, err
		}
		child = row.Live
		if a.ring, err = a.probe(); err != nil {
			return nil, err
		}
	} else {
		child = a.s.GroupAlive()
	}
	return map[string]any{
		"serve": a.serve, "child": child, "turn": turn, "disk": disk, "lines": lines,
		"ring": a.ring, "es": a.es, "timer": a.timer, "paused": a.paused, "row": a.row,
		"poll": "slow",
	}, nil
}

// probe is whether a new subscriber's replay would arm the catch-up:
// the spec's ring. It reads until the first frame that does, or until
// the replay is over.
func (a *eventStreamLifecycleAdapter) probe() (bool, error) {
	st, err := ltsOpen(a.s, a.live)
	if err != nil {
		return false, err
	}
	defer st.close()
	for {
		select {
		case f, ok := <-st.frames:
			if !ok {
				return false, errors.New("the probe's stream ended during its replay")
			}
			if eslArms(f) {
				return true, nil
			}
		case <-time.After(ltsQuiet):
			return false, nil
		}
	}
}

// eslUnits counts the spec's entries in a sequence of history kinds (a
// file's entries or the page's lines, which are those entries) and says
// whether a turn is open at the end. An input is not one: a prompt is
// the input and its turn's first tool call together (see above).
func eslUnits(kinds []string) (n int, open bool) {
	closedBy := ""
	for _, k := range kinds {
		switch k {
		case "input":
			open, closedBy = true, ""
		case "call":
			if open {
				n++
			}
		case "cancelled":
			open, closedBy = false, "cancelled"
		case "done":
			// The loop writes a done after every cancel: bookkeeping.
			if !open && closedBy == "cancelled" {
				continue
			}
			if open {
				n++
			}
			open, closedBy = false, "done"
		}
	}
	return n, open
}

// units is disk as the adapter's own steps wait for it.
func (a *eventStreamLifecycleAdapter) units() (int, error) {
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.live+".jsonl"))
	if err != nil {
		return 0, err
	}
	var kinds []string
	for _, e := range entries {
		kinds = append(kinds, e.Kind)
	}
	n, _ := eslUnits(kinds)
	return n, nil
}

func (a *eventStreamLifecycleAdapter) waitUnits(want int) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		n, err := a.units()
		if err != nil {
			return err
		}
		if n >= want {
			return nil
		}
		if time.Now().After(deadline) {
			entries, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.live+".jsonl"))
			b, _ := json.Marshal(entries)
			return fmt.Errorf("history holds %d entries, not %d, after %s: %s", n, want, actionTimeout, b)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// status is the session's status as serve derives it now.
func (a *eventStreamLifecycleAdapter) status() (string, bool, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.live)
	if err != nil {
		return "", false, err
	}
	return string(row.Status), row.Live, nil
}

// facts is the spec's child, turn and disk, read where the spec's
// requires need them.
func (a *eventStreamLifecycleAdapter) facts() (child bool, open bool, disk int, err error) {
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.live+".jsonl"))
	if err != nil {
		return false, false, 0, err
	}
	var kinds []string
	for _, e := range entries {
		kinds = append(kinds, e.Kind)
	}
	disk, open = eslUnits(kinds)
	if a.up {
		_, child, err = a.status()
	}
	return child, open, disk, err
}

// reach is the spec's up(serve): the page reaches a serve.
func (a *eventStreamLifecycleAdapter) reach() bool { return a.serve == "a" || a.serve == "b" }

// --- the child and the person's prompt ---

func (a *eventStreamLifecycleAdapter) Record() error {
	child, open, disk, err := a.facts()
	if err != nil {
		return err
	}
	if !a.gate.pass(child && open && disk < 3) {
		return nil
	}
	if err := a.call(); err != nil {
		return err
	}
	if err := a.waitUnits(disk + 1); err != nil {
		return err
	}
	return a.await("the call's frame")
}

// call releases the held request as one bash tool call and holds the
// request that follows it.
func (a *eventStreamLifecycleAdapter) call() error {
	next := a.nextTurn()
	control.Queue(a.t, a.dir, next, control.Turn{Mode: "block", Text: "finished " + next})
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Bash: "true # " + a.held})
	a.held = next
	return a.taken(next)
}

// taken is control.WaitTaken as an error: inside a walk the step that
// hung is the report.
func (a *eventStreamLifecycleAdapter) taken(name string) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(filepath.Join(a.dir, name+".taken")); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			entries, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.live+".jsonl"))
			var kinds []string
			for _, e := range entries {
				kinds = append(kinds, e.Kind)
			}
			return fmt.Errorf("llm-control: turn %q not taken after %s; history: %v", name, actionTimeout, kinds)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (a *eventStreamLifecycleAdapter) Finish() error {
	child, open, disk, err := a.facts()
	if err != nil {
		return err
	}
	if !a.gate.pass(child && open && disk < 3) {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if _, err := waitRow(a.s, a.live, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	if err := a.waitUnits(disk + 1); err != nil {
		return err
	}
	return a.await("the done frame")
}

// NextPrompt is the composer: POST /prompt, which adopts the session
// with `-r` when no child holds it.
func (a *eventStreamLifecycleAdapter) NextPrompt() error {
	child, open, disk, err := a.facts()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.reach() && (!open || !child) && disk < 3) {
		return nil
	}
	name := a.nextTurn()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.live, "turn "+name); err != nil {
		return err
	}
	a.held = name
	if err := a.taken(name); err != nil {
		return err
	}
	if err := a.call(); err != nil {
		return err
	}
	if err := a.waitUnits(disk + 1); err != nil {
		return err
	}
	return a.await("the call's frame")
}

// --- serve ---

// SigTerm is the page's half of launchd's or `bough update`'s SIGTERM
// (see the abstraction above): the proxy loses serve, so the page's
// stream ends and every request it makes is a 502. The witness is a
// stream on serve A itself, which CloseKills' real SIGTERM must end.
func (a *eventStreamLifecycleAdapter) SigTerm() error {
	a.pump()
	if !a.gate.pass(a.serve == "a") {
		return nil
	}
	w, err := ltsOpen(a.s, a.live)
	if err != nil {
		return err
	}
	a.witness = w
	a.proxy.down.Store(true)
	a.serve = "draining"
	if a.stream != nil {
		a.stream.close()
		a.stream, a.es = nil, "connecting"
	}
	return nil
}

// CloseKills is the real SIGTERM: Shutdown ends every stream, then Close
// kills the child, and serve exits.
func (a *eventStreamLifecycleAdapter) CloseKills() error {
	a.pump()
	if !a.gate.pass(a.serve == "draining") {
		return nil
	}
	a.s.Shutdown()
	a.up, a.serve, a.held = false, "down", ""
	w := a.witness
	a.witness = nil
	deadline := time.After(2 * time.Second)
	for {
		select {
		case _, ok := <-w.frames:
			if !ok {
				return nil
			}
		case <-deadline:
			w.close()
			return errors.New("an /events stream on serve A outlived its exit")
		}
	}
}

func (a *eventStreamLifecycleAdapter) ServeStart() error {
	a.pump()
	if !a.gate.pass(a.serve == "down") {
		return nil
	}
	if err := a.s.Resume(""); err != nil {
		return err
	}
	a.up, a.serve = true, "b"
	a.proxy.down.Store(false)
	return nil
}

// --- the network and the EventSource ---

func (a *eventStreamLifecycleAdapter) NetworkDrop() error {
	a.pump()
	if !a.gate.pass(a.es == "open" && a.reach()) {
		return nil
	}
	a.stream.close()
	a.stream, a.es = nil, "connecting"
	return nil
}

// EventSourceRetry is the EventSource's own reconnect.
func (a *eventStreamLifecycleAdapter) EventSourceRetry() error {
	a.pump()
	if !a.gate.pass(a.es == "connecting" && a.reach()) {
		return nil
	}
	return a.open()
}

// EventsNon200 is a reconnect answered 502: by the proxy while serve is
// down, or by a glitch of it while serve is up. The EventSource is then
// CLOSED, and the page, listening only for messages, does nothing.
func (a *eventStreamLifecycleAdapter) EventsNon200() error {
	a.pump()
	if !a.gate.pass(a.es == "connecting") {
		return nil
	}
	if a.reach() {
		a.proxy.glitch.Store(true)
	}
	st, code, err := a.connect()
	if err != nil {
		return fmt.Errorf("the reconnect failed on the network, not with a status: %w", err)
	}
	if code == http.StatusOK {
		st.close()
		return errors.New("the reconnect was answered 200")
	}
	a.es = "closed"
	return nil
}

// --- the page ---

// CatchUp is the armed timer firing.
func (a *eventStreamLifecycleAdapter) CatchUp() error {
	a.pump()
	if !a.gate.pass(a.timer) {
		return nil
	}
	return a.catchUp()
}

// ListPoll is the list read: the row takes serve's status.
func (a *eventStreamLifecycleAdapter) ListPoll() error {
	a.pump()
	enabled := false
	var st string
	if a.reach() {
		var err error
		if st, _, err = a.status(); err != nil {
			return err
		}
		enabled = a.row != st
	}
	if !a.gate.pass(enabled) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	resp, err := a.request(ctx, "/api/sessions")
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	var r struct {
		Sessions []serve.Row `json:"sessions"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&r); err != nil {
		return err
	}
	for _, row := range r.Sessions {
		if row.ID == a.live {
			a.row = string(row.Status)
			return nil
		}
	}
	return errors.New("the session is not in the list")
}

// Retry is ThreadStripRetry: the catch-up now, nothing else.
func (a *eventStreamLifecycleAdapter) Retry() error {
	a.pump()
	if !a.gate.pass(a.paused) {
		return nil
	}
	if a.retryResubscribes && a.reach() && a.es != "open" {
		if err := a.open(); err != nil {
			return err
		}
	}
	return a.catchUp()
}

// Reselect is selecting the thread again, or reloading the page.
func (a *eventStreamLifecycleAdapter) Reselect() error {
	a.pump()
	if !a.gate.pass(a.reach()) {
		return nil
	}
	return a.selectThread()
}

var eventStreamLifecycleActions = map[string]func(*eventStreamLifecycleAdapter) error{
	"Record":           (*eventStreamLifecycleAdapter).Record,
	"Finish":           (*eventStreamLifecycleAdapter).Finish,
	"NextPrompt":       (*eventStreamLifecycleAdapter).NextPrompt,
	"SigTerm":          (*eventStreamLifecycleAdapter).SigTerm,
	"CloseKills":       (*eventStreamLifecycleAdapter).CloseKills,
	"ServeStart":       (*eventStreamLifecycleAdapter).ServeStart,
	"NetworkDrop":      (*eventStreamLifecycleAdapter).NetworkDrop,
	"EventSourceRetry": (*eventStreamLifecycleAdapter).EventSourceRetry,
	"EventsNon200":     (*eventStreamLifecycleAdapter).EventsNon200,
	"CatchUp":          (*eventStreamLifecycleAdapter).CatchUp,
	"ListPoll":         (*eventStreamLifecycleAdapter).ListPoll,
	"Retry":            (*eventStreamLifecycleAdapter).Retry,
	"Reselect":         (*eventStreamLifecycleAdapter).Reselect,
}

// walk drives one generated path and compares the Stream role's state
// after every step. Every action on a path is enabled in the spec, so
// the gate closing is itself a mismatch.
func (a *eventStreamLifecycleAdapter) walk(p []tracecheck.Step) error {
	for i, st := range p {
		name := strings.TrimPrefix(st.Action, "Stream#0.")
		var err error
		if st.Action == "Init" {
			err = a.Init()
		} else if f, ok := eventStreamLifecycleActions[name]; ok {
			err = f(a)
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
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		if d := eslDiff(st.State, got); d != "" {
			return fmt.Errorf("step %d (%s): %s", i, name, d)
		}
	}
	return nil
}

// eslDiff compares the spec's Stream#0 fields with the adapter's state,
// through JSON so ints match the graph's numbers.
func eslDiff(want, got map[string]any) string {
	var d []string
	for k, v := range want {
		field, ok := strings.CutPrefix(k, "Stream#0.")
		if !ok {
			continue
		}
		wj, _ := json.Marshal(v)
		hj, _ := json.Marshal(got[field])
		if string(wj) != string(hj) {
			d = append(d, fmt.Sprintf("%s: spec %s, adapter %s", field, wj, hj))
		}
	}
	if len(d) == 0 {
		return ""
	}
	gj, _ := json.Marshal(got)
	return strings.Join(d, "; ") + "\n  adapter state: " + string(gj)
}

// eventStreamLifecycleHistory reads the session's transcript as the
// spec's steps: disk and turn are what a history file shows. The first
// tool call is Init, a prompt's first tool call is NextPrompt, any other
// is Record. A cancelled that closes an open turn is the `-r` resume on
// serve B, so SigTerm, CloseKills and ServeStart came before the input
// that follows it. A tail recorded while draining reads as recorded
// before the SIGTERM, a path the spec has too.
func eventStreamLifecycleHistory(entries []history.Entry) []tracecheck.Step {
	st := func(disk int, open bool) map[string]any {
		turn := "closed"
		if open {
			turn = "open"
		}
		return map[string]any{"Stream#0.disk": disk, "Stream#0.turn": turn}
	}
	var steps []tracecheck.Step
	add := func(action string, disk int, open bool) {
		if len(steps) == 0 {
			return // before Init's tool call: no state of the spec
		}
		steps = append(steps, tracecheck.Step{Action: "Stream#0." + action, State: st(disk, open)})
	}
	n, open, closedBy, prompted := 0, false, "", false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			open, closedBy, prompted = true, "", true
		case "call":
			if !open {
				continue
			}
			n++
			switch {
			case len(steps) == 0:
				steps = append(steps, tracecheck.Step{Action: "Init", State: st(n, true)})
			case prompted:
				add("NextPrompt", n, true)
			default:
				add("Record", n, true)
			}
			prompted = false
		case "cancelled":
			if open {
				for _, act := range []string{"SigTerm", "CloseKills", "ServeStart"} {
					add(act, n, true)
				}
			}
			open, closedBy = false, "cancelled"
		case "done":
			if !open && closedBy == "cancelled" {
				continue
			}
			if open {
				n++
				add("Finish", n, false)
			}
			open, closedBy = false, "done"
		}
	}
	return steps
}

func init() { historyProjections["event_stream_lifecycle"] = eventStreamLifecycleHistory }

func eslTestdata() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "event_stream_lifecycle")
}

func eslWalks(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	raw, err := pathsJSONCover("event_stream_lifecycle", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range doc.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// TestEventStreamLifecyclePaths walks every generated walk (every
// settled state; MODEL_COVER=transitions, every link) against real
// serves, four at once, each walk on a fresh session, and checks every
// transcript the walks wrote against the graph. ESL_PATHS=n walks an
// evenly spaced sample of n.
func TestEventStreamLifecyclePaths(t *testing.T) {
	t.Parallel()
	walks := eslWalks(t, envCover())
	pick := make([]int, len(walks))
	for i := range pick {
		pick[i] = i
	}
	if k, err := strconv.Atoi(os.Getenv("ESL_PATHS")); err == nil && k > 0 && k < len(walks) {
		pick = pick[:0]
		for i := range k {
			pick = append(pick, i*len(walks)/k)
		}
	}
	g, err := tracecheck.Load(eslTestdata())
	if err != nil {
		t.Fatal(err)
	}
	const workers = 4
	for w := range workers {
		t.Run(fmt.Sprintf("serve%d", w), func(t *testing.T) {
			t.Parallel()
			a := newEventStreamLifecycleAdapter(t)
			for j := w; j < len(pick); j += workers {
				if err := a.walk(walks[pick[j]]); err != nil {
					var names []string
					for _, st := range walks[pick[j]] {
						names = append(names, strings.TrimPrefix(st.Action, "Stream#0."))
					}
					t.Errorf("walk %d: %v\n  walk: %s", pick[j], err, strings.Join(names, " "))
				}
			}
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), eventStreamLifecycleHistory)
			}
		})
	}
}

// The walk proves nothing unless a wrong page fails it. The bug, Retry
// opening a new EventSource, shows only on a Retry with the stream not
// open and serve up: the walks over every link that take one.
func TestEventStreamLifecyclePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	var hit [][]tracecheck.Step
	for _, w := range eslWalks(t, tracecheck.CoverTransitions) {
		prev := w[0].State
		for _, st := range w[1:] {
			if st.Action == "Stream#0.Retry" && prev["Stream#0.es"] != "open" &&
				(prev["Stream#0.serve"] == "a" || prev["Stream#0.serve"] == "b") {
				hit = append(hit, w)
				break
			}
			prev = st.State
		}
		if len(hit) == 4 {
			break
		}
	}
	if len(hit) == 0 {
		t.Fatal("no walk takes Retry with the stream down and serve up")
	}
	a := newEventStreamLifecycleAdapter(t)
	a.retryResubscribes = true
	for _, w := range hit {
		if err := a.walk(w); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatalf("%d walks passed with a Retry that re-subscribes; the walk is not checking state", len(hit))
}

// The projection reads a resume after a restart as SigTerm, CloseKills
// and ServeStart, and a transcript with more entries than the spec's
// MAXDISK is not a path in it.
func TestEventStreamLifecycleHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(eslTestdata())
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	resumed := []history.Entry{e("meta"), e("input"), e("call"), e("call"), e("cancelled"), e("done"), e("input"), e("call")}
	if v := g.Check(eventStreamLifecycleHistory(resumed)); v != nil {
		t.Fatalf("a resume after a restart: %v", v)
	}
	finished := []history.Entry{e("meta"), e("input"), e("call"), e("done"), e("input"), e("call")}
	if v := g.Check(eventStreamLifecycleHistory(finished)); v != nil {
		t.Fatalf("a second prompt after a finish: %v", v)
	}
	over := []history.Entry{e("meta"), e("input"), e("call"), e("call"), e("call"), e("call")}
	if v := g.Check(eventStreamLifecycleHistory(over)); v == nil {
		t.Fatal("a fourth entry passed the trace check past MAXDISK")
	}
}
