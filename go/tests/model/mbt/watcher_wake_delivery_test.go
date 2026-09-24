//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/watcher_wake_delivery.fizz against serve: one watcher, the one
// session it wakes, and the serve lifecycle around them.
//
// serve runs in process (supervisor, API behind httptest, the watcher
// engine from API.WatchEngine), because the engine's poll and deliver
// run on one goroutine a second apart and the spec takes them apart:
// here the adapter calls Tick itself, on the engine's own clock, and the
// engine's two calls into serve (Idle, then Wake per queued text) park at
// a gate until the walk's IdleCheck or Send lets them run for real. The
// rest is real: the watcher file evaluated by codemode, its command run
// by /bin/sh, the supervisor's Idle and Send, and a real bough child per
// session answering through llm-control. Signal, Close and Restart are
// serveForeground's order by hand: the API goes (httptest closes), then
// sup.Close with the engine's last deliver still running, then a new
// supervisor, API and engine on the same HOME.
//
// Fields: queued, failing and cool are read off the engine's Status (the
// watcher row GET /api/hooks shows), woken off the gate's Wake results;
// status, kid and archived off the session's row (history and the
// supervisor while the API is down); serve and world are the adapter's
// own (it is the process and the outside world); pending is what the
// deliver took, checked against each Wake it makes; prev and last_text
// are the engine's private memory, so they are the last output a poll
// succeeded on and the last text seen joining the queue, and are proved
// by what later polls queue.
//
// The walks are the graph's, restricted to what the one engine goroutine
// can do: a tick that polled delivers before the next tick, so no poll
// follows while the idle session could have taken the queue or while
// deliver is still sending; and a watcher due (every < MinGap, so after
// any Cooldown) polls in the tick that delivers. wwdUndrivable names each
// such step; wwdWalks covers the graph without them, and the walk test
// logs how many targets that leaves out.

const (
	wwdRole  = "Watch#0."
	wwdEvery = time.Minute // the watcher's interval
	wwdGap   = time.Hour   // the engine's MinGap: longer than every, as in production
)

// wwdWatcher wakes on "the output changed and says there is news".
var wwdWatcher = `if (event.phase === "config") return { every: "1m", run: "cat ` + "%s" + `" }
if (event.now !== event.prev && event.now > 0) return { wake: "news " + event.now }
return {}
`

// wwdField reads one of the role's fields off a graph state.
func wwdField(s map[string]any, k string) any { return s[wwdRole+k] }

func wwdLen(s map[string]any, k string) int {
	l, _ := wwdField(s, k).([]any)
	return len(l)
}

// wwdIdleCheckEnabled is IdleCheck's require.
func wwdIdleCheckEnabled(s map[string]any) bool {
	return wwdField(s, "serve") != "down" && wwdLen(s, "pending") == 0 && wwdLen(s, "queued") > 0 &&
		wwdField(s, "archived") == false && wwdField(s, "status") == "idle"
}

// wwdCtx is what the real engine carries that the spec's state does not:
// whether the last tick's deliver is still parked at its Idle call (the
// spec's window between a poll and its deliver), and whether the watcher
// is due, so the next tick polls.
type wwdCtx struct {
	held, due bool
}

// errUndrivable marks a step the real system cannot take from here.
var errUndrivable = errors.New("undrivable")

// wwdUndrivable says why a step cannot happen in the real engine from
// here, or "" when it can. The spec splits a tick into Poll, IdleCheck
// and Send so that a person's step can land between them; it also lets
// two engine steps land between them, which one goroutine cannot do.
func wwdUndrivable(name string, s0 map[string]any, c wwdCtx) string {
	switch name {
	case "Poll", "PollFail":
		if wwdLen(s0, "pending") > 0 {
			return "the engine polls again only after deliver has sent everything it took"
		}
		if c.held && wwdIdleCheckEnabled(s0) {
			return "the tick that polled delivers to the idle session before the next poll"
		}
	case "IdleCheck":
		if !c.held && c.due && (wwdField(s0, "world") != wwdField(s0, "prev") || wwdField(s0, "failing") == true) {
			return "the watcher is due, so this tick polls first, and that poll would change something"
		}
	case "SendFail":
		if wwdField(s0, "archived") == false && wwdField(s0, "status") != "needs-you" && wwdField(s0, "kid") == true {
			return "nothing makes a write into a live child's stdin fail"
		}
	}
	return ""
}

// wwdAfter is c after the step: a poll's deliver stays parked while the
// session could take the queue, and is let go once it could not.
func wwdAfter(name string, s1 map[string]any, c wwdCtx) wwdCtx {
	switch name {
	case "Poll", "PollFail":
		c.due, c.held = false, wwdIdleCheckEnabled(s1)
	case "IdleCheck":
		c.due, c.held = false, false
	case "Cooldown", "Restart":
		c.due = true
	case "Close":
		c.held = false
	}
	if c.held && !wwdIdleCheckEnabled(s1) {
		c.held = false
	}
	return c
}

type wwdStep struct {
	link     int
	name     string
	src, dst int
}

type wwdNode struct {
	n int
	c wwdCtx
}

// wwdWalks covers the graph the way tracecheck.Walks does, over the
// graph times wwdCtx, taking only drivable steps. missed counts the
// targets (links, or settled states) no drivable walk reaches.
func wwdWalks(g *tracecheck.Graph, cover tracecheck.Cover) (walks [][]wwdStep, missed int) {
	const maxSteps = 50
	out := map[int][]int{}
	for i, l := range g.Links {
		if l.Type == "action" {
			out[l.Src] = append(out[l.Src], i)
		}
	}
	edges := func(p wwdNode) (res []wwdStep, next []wwdNode) {
		for _, li := range out[p.n] {
			l := g.Links[li]
			name := strings.TrimPrefix(l.Name, wwdRole)
			if wwdUndrivable(name, g.Nodes[l.Src].State, p.c) != "" {
				continue
			}
			res = append(res, wwdStep{li, name, l.Src, l.Dest})
			next = append(next, wwdNode{l.Dest, wwdAfter(name, g.Nodes[l.Dest].State, p.c)})
		}
		return
	}
	key := func(s wwdStep) int {
		if cover == tracecheck.CoverTransitions {
			return s.link
		}
		return s.dst
	}
	start := wwdNode{0, wwdCtx{due: true}}
	want := map[int]bool{}
	seen := map[wwdNode]bool{start: true}
	queue := []wwdNode{start}
	for len(queue) > 0 {
		p := queue[0]
		queue = queue[1:]
		steps, next := edges(p)
		for i, s := range steps {
			want[key(s)] = true
			if !seen[next[i]] {
				seen[next[i]] = true
				queue = append(queue, next[i])
			}
		}
	}
	targets := map[int]bool{}
	for i, l := range g.Links {
		if l.Type == "action" {
			targets[key(wwdStep{link: i, dst: l.Dest})] = true
		}
	}
	missed = len(targets) - len(want)
	// nearest is the shortest drivable chain from p ending in a wanted step.
	nearest := func(p wwdNode) []wwdStep {
		type back struct {
			from wwdNode
			step wwdStep
		}
		parent := map[wwdNode]back{}
		seen := map[wwdNode]bool{p: true}
		queue := []wwdNode{p}
		for len(queue) > 0 {
			cur := queue[0]
			queue = queue[1:]
			steps, next := edges(cur)
			for i, s := range steps {
				if want[key(s)] {
					res := []wwdStep{s}
					for c := cur; c != p; c = parent[c].from {
						res = append([]wwdStep{parent[c].step}, res...)
					}
					return res
				}
				if !seen[next[i]] {
					seen[next[i]] = true
					parent[next[i]] = back{cur, s}
					queue = append(queue, next[i])
				}
			}
		}
		return nil
	}
	for len(want) > 0 {
		cur := start
		var walk []wwdStep
		for len(walk) < maxSteps {
			hit := nearest(cur)
			if len(hit) == 0 || (len(walk) > 0 && len(walk)+len(hit) > maxSteps) {
				break
			}
			for _, s := range hit {
				delete(want, key(s))
				cur = wwdNode{s.dst, wwdAfter(s.name, g.Nodes[s.dst].State, cur.c)}
			}
			walk = append(walk, hit...)
		}
		if len(walk) == 0 {
			break
		}
		walks = append(walks, walk)
	}
	return walks, missed
}

// ---- the gate between the engine and serve ----

// wwdCall is one of the engine's calls into serve, parked until the walk
// lets it run.
type wwdCall struct {
	wake    bool
	text    string
	release chan struct{}
	done    chan struct{}
	idle    bool
	err     error
}

type wwdGate struct {
	wake  watch.Waker
	busy  watch.Idler
	calls chan *wwdCall
}

func (g *wwdGate) park(c *wwdCall) *wwdCall {
	c.release, c.done = make(chan struct{}), make(chan struct{})
	g.calls <- c
	<-c.release
	return c
}

func (g *wwdGate) Idle(session string) bool {
	c := g.park(&wwdCall{})
	c.idle = g.busy.Idle(session)
	close(c.done)
	return c.idle
}

func (g *wwdGate) Wake(session, text string) error {
	c := g.park(&wwdCall{wake: true, text: text})
	c.err = g.wake.Wake(session, text)
	close(c.done)
	return c.err
}

// ---- the adapter ----

type wwdAdapter struct {
	t                     *testing.T
	root, home, hist, ctl string
	exe                   string // the children's binary, behind a script SendFail makes unrunnable
	worldPath             string
	env                   []string

	sup     *serve.Supervisor
	api     *serve.API
	srv     *httptest.Server // nil from Signal on
	e       *watch.Engine
	gate    *wwdGate
	ticking chan struct{} // closed when the running Tick returns; nil when none
	call    *wwdCall      // the engine's call parked at the gate
	loaded  time.Time

	clockMu sync.Mutex
	clock   time.Time

	serve          string
	world          int
	prev, lastText int
	pending, woken []int
	downFailing    bool // what /api/hooks said last, while there is no API

	id   string
	n    int
	turn int
	next string // the block turn queued for the session's next request
	held string // the request held in flight, "" when none

	did    map[string]int
	reasks int // walks stopped at a re-ask
	askAt  int // history length when the last Ask step began

	// coolNeverEnds is the wrong adapter's bug: Cooldown lets no time pass.
	coolNeverEnds bool
}

func newWWDAdapter(t *testing.T) *wwdAdapter {
	// A short root under the system temp dir, like servetest: a
	// session's unix sockets live under HOME.
	root, err := os.MkdirTemp("", "bwwd-")
	if err != nil {
		t.Fatal(err)
	}
	root, _ = filepath.EvalSymlinks(root)
	a := &wwdAdapter{t: t, root: root, home: filepath.Join(root, "home"), did: map[string]int{},
		clock: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	a.hist = filepath.Join(a.home, ".bough", "history")
	a.ctl = control.Dir(a.home)
	a.worldPath = filepath.Join(root, "world")
	a.exe = filepath.Join(root, "bough.sh")
	files := map[string]string{
		filepath.Join(a.home, ".bough", "bough.yml"):           controlConfig,
		filepath.Join(a.home, ".bough", "watchers", "news.js"): fmt.Sprintf(wwdWatcher, a.worldPath),
		a.exe: "#!/bin/sh\nexec '" + servetest.Binary(t) + "' \"$@\"\n",
	}
	for p, body := range files {
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	os.MkdirAll(a.hist, 0o755)
	os.MkdirAll(a.ctl, 0o755)
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if k == "HOME" || k == "BOUGH_WEB_ADDR" || k == "BOUGH_BIN" || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		a.env = append(a.env, kv)
	}
	a.env = append(a.env, "HOME="+a.home, "BOUGH_WEB_ADDR=127.0.0.1:0")
	// Registered first, so it runs last: nothing may outlive HOME.
	t.Cleanup(func() { os.RemoveAll(root) })
	t.Cleanup(func() {
		if a.sup != nil && a.serve != "down" {
			a.shutdown()
		}
	})
	return a
}

func (a *wwdAdapter) now() time.Time {
	a.clockMu.Lock()
	defer a.clockMu.Unlock()
	return a.clock
}

func (a *wwdAdapter) advanceTo(t time.Time) {
	a.clockMu.Lock()
	defer a.clockMu.Unlock()
	if t.After(a.clock) {
		a.clock = t
	}
}

// up starts a serve on HOME: supervisor, API, and a freshly loaded
// engine, as serveForeground does.
func (a *wwdAdapter) up() error {
	sup, err := serve.NewSupervisor(serve.Options{
		Exe: a.exe, HistDir: a.hist, MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Env: a.env, Home: a.home, Runtime: container.NewFake(),
	})
	if err != nil {
		return err
	}
	a.sup, a.api = sup, serve.NewAPI(sup)
	a.srv = httptest.NewServer(a.api)
	a.serve = "up"
	e := a.api.WatchEngine()
	e.Dir = filepath.Join(a.home, ".bough", "watchers")
	e.Now, e.MinGap = a.now, wwdGap
	a.gate = &wwdGate{wake: e.Wake, busy: e.Busy, calls: make(chan *wwdCall)}
	e.Wake, e.Busy = a.gate, a.gate
	a.e, a.ticking, a.call = e, nil, nil
	a.loaded = a.now()
	a.prev, a.lastText, a.pending, a.woken = -1, -1, nil, nil
	if err := e.Load(context.Background()); err != nil {
		return err
	}
	if w := a.watcher(); w.Failing || w.Every != wwdEvery.String() {
		return fmt.Errorf("the watcher did not load: %+v", w)
	}
	return nil
}

// shutdown is Close: sup.Close with the engine's tick still running, as
// the deferred stopWatch only cancels it; what it still delivers meets
// a closed supervisor.
func (a *wwdAdapter) shutdown() error {
	if a.srv != nil {
		a.srv.Close()
		a.srv = nil
	}
	a.downFailing = a.watcher().Failing
	a.sup.Close()
	for a.call != nil {
		if _, err := a.release(); err != nil {
			return err
		}
	}
	a.serve, a.pending, a.held = "down", nil, ""
	if a.id != "" {
		return a.waitFor("no child after Close", func() bool { return !a.sup.Live(a.id) })
	}
	return nil
}

func (a *wwdAdapter) watcher() watch.WatcherStatus {
	st := a.e.Status()
	if len(st) != 1 {
		a.t.Fatalf("want one watcher, got %+v", st)
	}
	return st[0]
}

// due is when the watcher's next poll is: at Load, then every after the
// last one.
func (a *wwdAdapter) due() time.Time {
	if w := a.watcher(); w.LastRun != nil {
		return w.LastRun.Add(wwdEvery)
	}
	return a.loaded
}

// ---- ticks ----

// settle waits until the running tick parks at the gate or returns.
func (a *wwdAdapter) settle() error {
	select {
	case c := <-a.gate.calls:
		a.call = c
	case <-a.ticking:
		a.ticking, a.call = nil, nil
	case <-time.After(actionTimeout):
		return errors.New("the watcher tick neither called serve nor returned")
	}
	return nil
}

func (a *wwdAdapter) tick() error {
	if a.ticking != nil {
		return errors.New("tick: the last tick is still running")
	}
	done := make(chan struct{})
	a.ticking = done
	e := a.e
	go func() { e.Tick(context.Background()); close(done) }()
	return a.settle()
}

// release lets the parked call run for real and waits for the tick's
// next call or its end.
func (a *wwdAdapter) release() (*wwdCall, error) {
	c := a.call
	a.call = nil
	close(c.release)
	select {
	case <-c.done:
	case <-time.After(actionTimeout):
		return c, errors.New("the engine's call into serve did not return")
	}
	return c, a.settle()
}

// ---- reading the state ----

type wwdView struct {
	status        string
	archived, kid bool
	queued        []int
	failing, cool bool
}

func (a *wwdAdapter) view() (wwdView, error) {
	var v wwdView
	w := a.watcher()
	v.failing = w.Failing
	v.cool = w.LastWoke != nil && a.now().Sub(*w.LastWoke) < wwdGap
	v.queued = []int{}
	if a.serve == "down" {
		// The engine and its memory are gone; nothing reads it.
		v.failing = a.downFailing
	} else {
		for _, q := range w.Queued {
			n, err := strconv.Atoi(strings.TrimPrefix(q, "news "))
			if err != nil {
				return v, fmt.Errorf("queued %q", q)
			}
			v.queued = append(v.queued, n)
		}
	}
	var st serve.Status
	if a.srv != nil {
		var r struct {
			Session serve.Row `json:"session"`
		}
		if err := a.get("/api/sessions/"+a.id, &r); err != nil {
			return v, err
		}
		st, v.archived, v.kid = r.Session.Status, r.Session.Archived, r.Session.Live
	} else {
		entries, err := a.sup.Entries(a.id)
		if err != nil {
			return v, err
		}
		v.kid = a.sup.Live(a.id)
		st, _ = serve.StatusOf(entries, v.kid)
		v.archived = a.sup.Meta(a.id).Archived
	}
	switch st {
	case serve.StatusRunning:
		v.status = "running"
	case serve.StatusNeedsYou:
		v.status = "needs-you"
	default:
		v.status = "idle"
	}
	return v, nil
}

func (a *wwdAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	pending, woken := append([]int{}, a.pending...), append([]int{}, a.woken...)
	return map[string]any{
		"serve": a.serve, "world": a.world, "prev": a.prev, "last_text": a.lastText,
		"cool": v.cool, "failing": v.failing, "queued": v.queued, "pending": pending, "woken": woken,
		"status": v.status, "archived": v.archived, "kid": v.kid,
	}, nil
}

func (v wwdView) idleCheckEnabled(serveState string, pending int) bool {
	return serveState != "down" && pending == 0 && len(v.queued) > 0 && !v.archived && v.status == "idle"
}

// ---- HTTP, as the page ----

func (a *wwdAdapter) get(path string, out any) error {
	resp, err := http.Get(a.srv.URL + path)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s: %s", path, resp.Status)
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

func (a *wwdAdapter) post(verb string, body any) error {
	b, _ := json.Marshal(body)
	resp, err := http.Post(a.srv.URL+"/api/sessions/"+a.id+"/"+verb, "application/json", bytes.NewReader(b))
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		var e struct {
			Error string `json:"error"`
		}
		json.NewDecoder(resp.Body).Decode(&e)
		return fmt.Errorf("POST %s: %s %s", verb, resp.Status, e.Error)
	}
	return nil
}

func (a *wwdAdapter) waitFor(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: not after %s", what, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

func (a *wwdAdapter) waitStatus(want string) error {
	var v wwdView
	err := a.waitFor("status "+want, func() bool {
		var err error
		v, err = a.view()
		return err == nil && v.status == want
	})
	if err != nil {
		return fmt.Errorf("%w (%+v; history tail %s)", err, v, a.tail())
	}
	return nil
}

// tail is the session's last few history entries, for a failure.
func (a *wwdAdapter) tail() string {
	es := a.entries()
	if len(es) > 8 {
		es = es[len(es)-8:]
	}
	var out []string
	for _, e := range es {
		b, _ := json.Marshal(e.Data)
		if len(b) > 160 {
			b = b[:160]
		}
		out = append(out, e.Kind+" "+string(b))
	}
	return strings.Join(out, " | ")
}

// ---- llm-control ----

func (a *wwdAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("t%05d", a.turn)
	control.Queue(a.t, a.ctl, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

// takeNext waits for the queued turn to be taken and holds it.
func (a *wwdAdapter) takeNext() error {
	if err := waitTaken(a.ctl, a.next); err != nil {
		return err
	}
	a.held = a.next
	a.queueNext()
	return nil
}

// sync moves held to the request really in flight: a resumed child's
// first request (the interrupted turn going on) can be dropped for the
// prompt that started it, and the one that replaced it takes the next
// queued turn. It reports whether held moved.
func (a *wwdAdapter) sync() bool {
	moved := false
	for {
		if _, err := os.Stat(filepath.Join(a.ctl, a.next+".taken")); err != nil {
			return moved
		}
		a.held, moved = a.next, true
		a.queueNext()
	}
}

func (a *wwdAdapter) entries() []history.Entry {
	es, _ := history.Read(filepath.Join(a.hist, a.id+".jsonl"))
	return es
}

// ---- Init ----

// Init ends the last walk's serve, starts a fresh one on the same HOME,
// archives the last walk's session (the watcher wakes the newest
// unarchived one) and creates this walk's, its first turn answered and
// its child gone: the spec starts idle with no child.
func (a *wwdAdapter) Init() error {
	if a.sup != nil && a.serve != "down" {
		if err := a.shutdown(); err != nil {
			return err
		}
	}
	if a.next != "" {
		os.Remove(filepath.Join(a.ctl, a.next+".json"))
		a.next = ""
	}
	a.held, a.askAt = "", 0
	a.world = 0
	if err := os.WriteFile(a.worldPath, []byte("0\n"), 0o644); err != nil {
		return err
	}
	if err := a.up(); err != nil {
		return err
	}
	if a.id != "" {
		if err := a.sup.SetArchived(a.id, true); err != nil {
			return err
		}
	}
	a.n++
	cwd := filepath.Join(a.root, fmt.Sprintf("w%d", a.n))
	if err := os.MkdirAll(cwd, 0o755); err != nil {
		return err
	}
	// Nothing queued: the model answers at once.
	id, err := a.sup.Create(serve.CreateOptions{Cwd: cwd, Prompt: fmt.Sprintf("walk %d", a.n)})
	if err != nil {
		return err
	}
	a.id = id
	if err := a.waitFor("the first turn to end", func() bool {
		st, _ := serve.StatusOf(a.entries(), true)
		return st == serve.StatusDone
	}); err != nil {
		return err
	}
	if err := a.sup.Kill(id); err != nil {
		return err
	}
	if err := a.waitFor("the first child to go", func() bool { return !a.sup.Live(id) }); err != nil {
		return err
	}
	a.queueNext()
	return nil
}

// ---- actions ----

func (a *wwdAdapter) News() error {
	a.world++
	return os.WriteFile(a.worldPath, []byte(fmt.Sprintf("%d\n", a.world)), 0o644)
}

// poll is a tick with the watcher due. A deliver still parked from the
// last tick runs first (it could only find the session busy, archived,
// or the queue empty: the walk says so). After the poll the deliver
// stays parked only while the idle session could take the queue: that
// is the spec's window between a poll and its deliver.
func (a *wwdAdapter) poll(fail bool) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if a.call != nil {
		if a.call.wake || v.idleCheckEnabled(a.serve, len(a.pending)) {
			return errUndrivable
		}
		if _, err := a.release(); err != nil {
			return err
		}
		if a.ticking != nil {
			return errors.New("the parked deliver did not end its tick")
		}
	}
	a.advanceTo(a.due())
	if fail {
		// The command fails: its input is gone for this one poll.
		aside := a.worldPath + ".aside"
		if err := os.Rename(a.worldPath, aside); err != nil {
			return err
		}
		defer os.Rename(aside, a.worldPath)
	}
	if err := a.tick(); err != nil {
		return err
	}
	if a.call == nil || a.call.wake {
		return errors.New("the tick did not ask whether the session is idle")
	}
	if w := a.watcher(); !fail && !w.Failing {
		a.prev = a.world
	}
	// last_text is set only by queueing: the newest text in the queue.
	v, err = a.view()
	if err != nil {
		return err
	}
	if n := len(v.queued); n > 0 {
		a.lastText = v.queued[n-1]
	}
	return nil
}

// letDeliverGo runs a parked Idle call once the session could not take
// the queue: it finds that and returns, as it would have at any moment
// since. Only while IdleCheck is enabled does the parked call matter.
func (a *wwdAdapter) letDeliverGo() error {
	if a.call == nil || a.call.wake || a.serve == "down" {
		return nil
	}
	v, err := a.view()
	if err != nil {
		return err
	}
	if v.idleCheckEnabled(a.serve, len(a.pending)) {
		return nil
	}
	c, err := a.release()
	if err != nil {
		return err
	}
	if c.idle && len(v.queued) > 0 {
		return fmt.Errorf("deliver found the session idle and took %v", v.queued)
	}
	return nil
}

func (a *wwdAdapter) Poll() error     { return a.poll(false) }
func (a *wwdAdapter) PollFail() error { return a.poll(true) }

// IdleCheck lets deliver ask: the parked call of the tick that polled,
// or a tick of its own when there is none (the watcher not due, or due
// with nothing new to see).
func (a *wwdAdapter) IdleCheck() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if a.call == nil {
		if !a.now().Before(a.due()) && (a.world != a.prev || v.failing) {
			return errUndrivable
		}
		polls := !a.now().Before(a.due())
		if err := a.tick(); err != nil {
			return err
		}
		if polls && !a.watcher().Failing {
			a.prev = a.world
		}
	}
	if a.call == nil || a.call.wake {
		return errors.New("the tick did not ask whether the session is idle")
	}
	taken := append([]int{}, v.queued...)
	c, err := a.release()
	if err != nil {
		return err
	}
	if !c.idle {
		return nil // the state check says what the session looked like
	}
	a.pending = taken
	if len(taken) > 0 && (a.call == nil || !a.call.wake || a.call.text != fmt.Sprintf("news %d", taken[0])) {
		return fmt.Errorf("deliver took %v but its first Wake is %+v", taken, a.call)
	}
	return nil
}

// send lets deliver's next Wake run; fail arranges for it to fail first.
func (a *wwdAdapter) send(fail bool) error {
	if a.call == nil || !a.call.wake || len(a.pending) == 0 {
		return fmt.Errorf("no Wake is parked (pending %v)", a.pending)
	}
	if want := fmt.Sprintf("news %d", a.pending[0]); a.call.text != want {
		return fmt.Errorf("Wake carries %q, want %q", a.call.text, want)
	}
	v, err := a.view()
	if err != nil {
		return err
	}
	if fail {
		switch {
		case v.archived || v.status == "needs-you":
			// Refused by serve anyway; the outcome is the same.
		case !v.kid:
			// The spawn fails.
			if err := os.Chmod(a.exe, 0); err != nil {
				return err
			}
			defer os.Chmod(a.exe, 0o755)
		default:
			return errUndrivable
		}
	}
	before := len(a.entries())
	c, err := a.release()
	if err != nil {
		return err
	}
	text := a.pending[0]
	a.pending = a.pending[1:]
	if c.err != nil {
		if !fail && !v.archived && v.status != "needs-you" {
			return fmt.Errorf("the wake failed: %v", c.err)
		}
		return nil
	}
	if fail {
		return errors.New("SendFail: the wake went through")
	}
	a.woken = append(a.woken, text)
	if v.status == "idle" {
		// A new turn, on the child it spawned or the one it had.
		return a.turnStarts()
	}
	// A steer into the running turn: it lands in history now.
	return a.waitFor("the wake's input", func() bool {
		es := a.entries()
		for i := before; i < len(es); i++ {
			if es[i].Kind == "input" && strings.HasSuffix(str(es[i].Data["text"]), c.text) {
				return true
			}
		}
		return false
	})
}

func (a *wwdAdapter) Send() error     { return a.send(false) }
func (a *wwdAdapter) SendFail() error { return a.send(true) }

// Cooldown lets MinGap pass since the last delivered wake.
func (a *wwdAdapter) Cooldown() error {
	w := a.watcher()
	if w.LastWoke == nil {
		return errors.New("Cooldown: nothing was ever woken")
	}
	if !a.coolNeverEnds {
		a.advanceTo(w.LastWoke.Add(wwdGap))
	}
	return nil
}

func (a *wwdAdapter) Prompt() error {
	if err := a.post("prompt", map[string]string{"text": fmt.Sprintf("prompt %d", a.turn)}); err != nil {
		return err
	}
	return a.turnStarts()
}

// errReask is the engine re-running the tools.ask of a turn that was
// killed (Archive, Close) when the session's next child starts: the
// harness restores the call from its own store, which still has it
// running, although history says the turn was interrupted. The spec
// has the turn gone. It is a finding outside this flow (the engine's
// resume, not the watcher), so the walk stops there and is counted.
var errReask = errors.New("the new child re-asked the killed turn's question")

// turnStarts waits for the turn a prompt or a wake opened: its model
// request held and the session running.
func (a *wwdAdapter) turnStarts() error {
	var v wwdView
	err := a.waitFor("the turn to start", func() bool {
		var err error
		v, err = a.view()
		_, taken := os.Stat(filepath.Join(a.ctl, a.next+".taken"))
		return a.reasked() || (err == nil && v.status == "running" && taken == nil)
	})
	if a.reasked() {
		return errReask
	}
	if err != nil {
		return fmt.Errorf("%w (%+v; history tail %s)", err, v, a.tail())
	}
	a.held = a.next
	a.queueNext()
	return nil
}

// reasked says whether the session's current child has asked a question
// no Ask step of this walk put to it: the killed turn's ask, run again.
func (a *wwdAdapter) reasked() bool {
	es := a.entries()
	start := -1
	for i, e := range es {
		if e.Kind == "engine" {
			start = i
		}
	}
	if start < 0 || a.askAt > start {
		return false
	}
	for _, e := range es[start:] {
		if e.Kind == "ask" {
			return true
		}
	}
	return false
}

// Finish lets the turn end: a wake that steered it makes one more
// request, which is released too.
func (a *wwdAdapter) Finish() error {
	a.sync()
	for {
		if a.held == "" {
			return errors.New("finish: no model request is held")
		}
		control.Release(a.t, a.ctl, a.held)
		a.held = ""
		deadline := time.Now().Add(actionTimeout)
		for a.held == "" {
			if time.Now().After(deadline) {
				return errors.New("finish: the turn neither ended nor asked again")
			}
			if _, err := os.Stat(filepath.Join(a.ctl, a.next+".taken")); err == nil {
				a.held = a.next
				a.queueNext()
				break
			}
			if st, _ := serve.StatusOf(a.entries(), true); st == serve.StatusDone {
				return a.waitStatus("idle")
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
}

func (a *wwdAdapter) Ask() error {
	a.askAt = len(a.entries())
	a.sync()
	if a.held == "" {
		return errors.New("ask: no model request is held")
	}
	ask := func() {
		control.ReleaseWith(a.t, a.ctl, a.held, control.Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "which?"}})
	}
	ask()
	var v wwdView
	err := a.waitFor("the ask to be armed", func() bool {
		// The released request was dropped and another took its place
		// (sync): that one asks.
		if a.sync() {
			ask()
		}
		var err error
		v, err = a.view()
		return err == nil && v.status == "needs-you" && a.sup.PendingAsk(a.id) != nil
	})
	a.held = ""
	if err != nil {
		return fmt.Errorf("%w (%+v; history tail %s)", err, v, a.tail())
	}
	return nil
}

func (a *wwdAdapter) Answer() error {
	p := a.sup.PendingAsk(a.id)
	if p == nil {
		return errors.New("answer: no ask is armed")
	}
	if err := a.post("answer", map[string]string{"text": "this one", "ask": p.ID}); err != nil {
		return err
	}
	if err := a.takeNext(); err != nil {
		return err
	}
	return a.waitStatus("running")
}

func (a *wwdAdapter) Archive() error {
	if err := a.post("archive", nil); err != nil {
		return err
	}
	a.held = ""
	return a.waitFor("the child to go", func() bool { return !a.sup.Live(a.id) })
}

func (a *wwdAdapter) Unarchive() error { return a.post("unarchive", nil) }

// Signal is srv.Shutdown: the API goes, the engine and children stay.
func (a *wwdAdapter) Signal() error {
	a.srv.Close()
	a.srv = nil
	a.serve = "stopping"
	return nil
}

func (a *wwdAdapter) Close() error { return a.shutdown() }

// Restart is the same binary on the same HOME: everything is loaded
// from files again.
func (a *wwdAdapter) Restart() error {
	if err := a.up(); err != nil {
		return err
	}
	return nil
}

var wwdActions = map[string]func(*wwdAdapter) error{
	"News": (*wwdAdapter).News, "Poll": (*wwdAdapter).Poll, "PollFail": (*wwdAdapter).PollFail,
	"IdleCheck": (*wwdAdapter).IdleCheck, "Send": (*wwdAdapter).Send, "SendFail": (*wwdAdapter).SendFail,
	"Cooldown": (*wwdAdapter).Cooldown, "Prompt": (*wwdAdapter).Prompt, "Finish": (*wwdAdapter).Finish,
	"Ask": (*wwdAdapter).Ask, "Answer": (*wwdAdapter).Answer, "Archive": (*wwdAdapter).Archive,
	"Unarchive": (*wwdAdapter).Unarchive, "Signal": (*wwdAdapter).Signal, "Close": (*wwdAdapter).Close,
	"Restart": (*wwdAdapter).Restart,
}

// ---- the walk ----

// check compares the adapter's state with the spec's after a step,
// polling briefly: a child's history lands a moment after its pipe.
func (a *wwdAdapter) check(want map[string]any) error {
	var got map[string]any
	var diff []string
	deadline := time.Now().Add(3 * time.Second)
	for {
		s, err := a.GetState()
		if err != nil {
			return err
		}
		b, _ := json.Marshal(s)
		got = nil
		json.Unmarshal(b, &got)
		diff = diff[:0]
		for k, v := range want {
			f, ok := strings.CutPrefix(k, wwdRole)
			if !ok {
				continue
			}
			if !reflect.DeepEqual(got[f], v) {
				diff = append(diff, fmt.Sprintf("%s: want %v, got %v", f, v, got[f]))
			}
		}
		if len(diff) == 0 || time.Now().After(deadline) {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if len(diff) > 0 {
		return errors.New(strings.Join(diff, "; "))
	}
	return nil
}

// walk drives one walk and checks the state after every step. cut is
// why it stopped early when the adapter found a step undrivable.
func (a *wwdAdapter) walk(g *tracecheck.Graph, w []wwdStep) (checked int, cut string, err error) {
	if err := a.Init(); err != nil {
		return 0, "", fmt.Errorf("Init: %w", err)
	}
	if err := a.check(g.Nodes[0].State); err != nil {
		return 0, "", fmt.Errorf("Init: %w", err)
	}
	for i, s := range w {
		err := wwdActions[s.name](a)
		if errors.Is(err, errUndrivable) {
			return checked, fmt.Sprintf("step %d (%s)", i+1, s.name), nil
		}
		if err == nil {
			a.did[s.name]++
			if err = a.letDeliverGo(); err == nil {
				err = a.check(g.Nodes[s.dst].State)
			}
		}
		if err != nil && a.reasked() {
			// errReask: a finding outside this flow, counted.
			a.reasks++
			a.t.Logf("walk stopped at step %d (%s): %v; history tail %s", i+1, s.name, errReask, a.tail())
			return checked, "", nil
		}
		if err != nil {
			return checked, "", fmt.Errorf("step %d (%s): %w", i+1, s.name, err)
		}
		checked++
	}
	return checked, "", nil
}

func wwdActs(w []wwdStep) string {
	var s []string
	for _, x := range w {
		s = append(s, x.name)
	}
	return strings.Join(s, " ")
}

// wwdPath is the walk taking the named actions from Init, each of which
// the graph must enable.
func wwdPath(t *testing.T, g *tracecheck.Graph, names ...string) []wwdStep {
	t.Helper()
	var w []wwdStep
	cur := 0
	for _, name := range names {
		found := false
		for i, l := range g.Links {
			if l.Src == cur && l.Type == "action" && l.Name == wwdRole+name {
				w = append(w, wwdStep{i, name, l.Src, l.Dest})
				cur, found = l.Dest, true
				break
			}
		}
		if !found {
			t.Fatalf("%s is not enabled after %s", name, wwdActs(w))
		}
	}
	return w
}

func loadWWDGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("watcher_wake_delivery")), "..", "testdata", "watcher_wake_delivery"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// TestWatcherWakeDeliveryWalks walks the graph (every settled state by
// default, every link with MODEL_COVER=transitions) against serve, then
// replays each walk's transcript on the graph.
func TestWatcherWakeDeliveryWalks(t *testing.T) {
	t.Parallel()
	g := loadWWDGraph(t)
	walks, missed := wwdWalks(g, envCover())
	steps := 0
	for _, w := range walks {
		steps += len(w)
	}
	t.Logf("%d walks, %d steps; %d %s targets only undrivable steps reach", len(walks), steps, missed, envCover())
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newWWDAdapter(t)
			checked, cut := 0, 0
			var traced []string // the sessions of whole walks
			for i := sh; i < len(walks); i += shards {
				reasks := a.reasks
				n, why, err := a.walk(g, walks[i])
				checked += n
				if err != nil {
					t.Errorf("walk %d [%s]: %v", i, wwdActs(walks[i]), err)
					continue
				}
				if why != "" {
					cut++
					t.Logf("walk %d cut short at %s: %s", i, why, wwdActs(walks[i]))
				} else if a.reasks == reasks {
					traced = append(traced, a.id)
				}
			}
			t.Logf("steps checked %d, walks cut short %d, stopped at a re-ask %d; actions %v", checked, cut, a.reasks, a.did)
			if cut > 0 {
				t.Errorf("%d walks were cut short: wwdUndrivable and the adapter disagree", cut)
			}
			for _, id := range traced {
				checkHistory(t, g, sessionHistory(t, a.home, id), wwdHistory)
			}
			t.Logf("trace-checked %d transcripts", len(traced))
		})
	}
}

// A walk whose Cooldown lets no time pass must fail: cool stays true.
func TestWatcherWakeDeliveryCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	g := loadWWDGraph(t)
	walks, _ := wwdWalks(g, tracecheck.CoverStates)
	a := newWWDAdapter(t)
	a.coolNeverEnds = true
	for i, w := range walks {
		if !strings.Contains(wwdActs(w), "Cooldown") {
			continue
		}
		_, _, err := a.walk(g, w)
		if err != nil {
			t.Logf("walk %d failed as it should: %v", i, err)
			return
		}
	}
	t.Fatal("every walk passed with a Cooldown that lets no time pass; the walk is not checking state")
}

// wwdEvent is one entry of a transcript the projection reads.
type wwdEvent struct {
	kind string // "prompt", "wake", "steer", "ask", "answer", "done", "killed"
	news int    // a wake's news
}

// wwdEvents reads the entries after the session's first turn (made
// before the spec's Init) as events.
func wwdEvents(entries []history.Entry) []wwdEvent {
	var out []wwdEvent
	started := false
	for _, e := range entries {
		if !started {
			started = e.Kind == "done"
			continue
		}
		switch e.Kind {
		case "input":
			text := str(e.Data["text"])
			if strings.HasPrefix(text, "[watcher]") {
				i := strings.LastIndex(text, "news ")
				n, _ := strconv.Atoi(strings.TrimSpace(text[i+len("news "):]))
				kind := "wake"
				if b, _ := e.Data["steer"].(bool); b {
					kind = "steer"
				}
				out = append(out, wwdEvent{kind: kind, news: n})
			} else {
				out = append(out, wwdEvent{kind: "prompt"})
			}
		case "ask":
			out = append(out, wwdEvent{kind: "ask"})
		case "ask/answer":
			out = append(out, wwdEvent{kind: "answer"})
		case "done":
			out = append(out, wwdEvent{kind: "done"})
		case "cancelled":
			if b, _ := e.Data["interrupted"].(bool); b {
				out = append(out, wwdEvent{kind: "killed"})
			}
		}
	}
	return out
}

// wwdHistory reads a transcript as the flow's steps. History shows the
// turns (a person's prompt, a wake starting a turn or steering one, an
// ask, its answer, the end) and a turn killed under it; the engine's
// steps and serve's life leave nothing. So the projection plays the spec
// forward and puts in the unseen steps one path needs: before a wake,
// News up to its text, Poll (after a Cooldown or a restart when the spec
// would not queue it otherwise), IdleCheck; a wake that steered a turn
// was taken with the turn's start (in the same IdleCheck, before the
// Prompt or with the wake that started it); a turn the next child
// closed as interrupted was killed (Archive, Unarchive: a restart would
// do too, and is put in only where a wake needs one). The check is on
// status.
func wwdHistory(entries []history.Entry) []tracecheck.Step {
	var steps []tracecheck.Step
	st := struct {
		status            string
		world, prev, last int
		cool              bool
		woken             map[int]bool
	}{status: "idle", prev: -1, last: -1, woken: map[int]bool{}}
	add := func(action string) {
		name := action
		if action != "Init" {
			name = wwdRole + action
		}
		steps = append(steps, tracecheck.Step{Action: name, State: map[string]any{wwdRole + "status": st.status}})
	}
	add("Init")
	restart := func() {
		add("Signal")
		st.status = "idle"
		add("Close")
		st.prev, st.last, st.cool, st.woken = -1, -1, false, map[int]bool{}
		add("Restart")
	}
	killed := func() {
		if st.status != "idle" {
			st.status = "idle"
			add("Archive")
			add("Unarchive")
		}
	}
	// queue puts texts in the queue and takes them in one IdleCheck.
	queue := func(texts []int) {
		for _, n := range texts {
			if st.woken[n] || n == st.last || (n == st.world && n == st.prev) {
				restart()
				break
			}
		}
		for _, n := range texts {
			for st.world < n {
				st.world++
				add("News")
			}
			if st.cool {
				st.cool = false
				add("Cooldown")
			}
			st.prev, st.last = n, n
			add("Poll")
		}
		add("IdleCheck")
	}
	send := func(n int) {
		st.woken[n], st.cool, st.status = true, true, "running"
		add("Send")
	}
	ev := wwdEvents(entries)
	// steers are the wakes that steer the turn ev[i] starts.
	steers := func(i int) []int {
		var out []int
		for _, e := range ev[i+1:] {
			if e.kind == "steer" {
				out = append(out, e.news)
				continue
			}
			if e.kind == "ask" || e.kind == "answer" {
				continue
			}
			break
		}
		return out
	}
	for i, e := range ev {
		switch e.kind {
		case "prompt":
			if s := steers(i); len(s) > 0 {
				queue(s)
			}
			st.status = "running"
			add("Prompt")
		case "wake":
			queue(append([]int{e.news}, steers(i)...))
			send(e.news)
		case "steer":
			send(e.news)
		case "ask":
			st.status = "needs-you"
			add("Ask")
		case "answer":
			st.status = "running"
			add("Answer")
		case "done":
			st.status = "idle"
			add("Finish")
		case "killed":
			killed()
		}
	}
	return steps
}

func init() { historyProjections["watcher_wake_delivery"] = wwdHistory }
