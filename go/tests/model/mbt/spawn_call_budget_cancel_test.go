//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/spawn_call_budget_cancel.fizz against a real serve: one live
// parent session, on the loop (code mode: tools.spawn and tools.spawnAll
// in a js block) or on the engine (native spawn calls), spawning
// foreground children and background agents.
//
// How each spec event is made real:
//
//   - The model is llm-control. Every turn carries a match: the parent's
//     turns answer only the parent (its first prompt names parent-w<n>),
//     a child's only the child whose task names its marker, a background
//     agent's only that agent. One held parent turn is always queued, so
//     whatever request the parent makes next is held.
//   - A spawn is the parent's held request released with a js block
//     (code mode) or with calls (engine). On the engine a reply's calls
//     run and the model is only asked again when one finishes, so every
//     reply there also carries a `true` bash call: its result makes the
//     next request, which is held, and the turn stays open.
//   - A child is running while its held turn is; Child<i>Done releases it
//     with its report, Child1LLMFails with a provider error, ChildSpawns
//     with a spawn of its own (a js block, or a native spawn call) and
//     queues its next turn.
//   - Done answers every parent request with text until the turn closes;
//     on the engine a child still running is adopted after turn_settle.
//     Esc is serve's interrupt.
//   - A background create goes to serve through a proxy (serve.pid names
//     it): GiveUp is the proxy dropping the request both ways, so the tool
//     reads an error and serve sees its client gone. serve holds the
//     create before and after it makes the child while
//     BOUGH_TEST_CREATE_HOLD_DIR says so: that is in_flight and created.
//
// What is read off the server: turn (the row); c1/c2 (the child's
// sub:start without a sub:done; adopted when a job names its spawn
// call); used (children that closed normally since the last turn close);
// fan and results (the last accepted spawnAll: its block's result, else
// its children's sub:done); grand (a sub:start for a grandchild's task);
// the background fields from serve's holds, its children listing and the
// parent's transcript. spawns is not exposed by workers: it is reported
// as live + used, and whether the counter is right shows in which spawns
// the product accepts (a spawn it refuses leaves the slot none, one it
// wrongly accepts fills it). engine is the adapter's own: the loop row
// it created the session with.

const scMaxBgAttempts = 2

func scConfig(mode string) string {
	loop := "- id: loop\n  plugin: loop\n"
	if mode == "unreal" {
		loop = "- id: loop\n  plugin: engine-unreal\n  config:\n    turn_settle: 1s\n"
	}
	return controlConfig + "- id: workers\n  plugin: workers\n  config:\n    max_spawns: 2\n" + loop
}

// scChild is a foreground child: its marker and its held turn.
type scChild struct {
	mark, turn string
}

type scAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's queue
	holds string // BOUGH_TEST_CREATE_HOLD_DIR
	proxy *scProxy
	gate  gate

	walk, q, n  int
	mode        string // "" until UseLoop/UseEngine
	id, cwd     string
	ids         []string
	pmark       string   // the parent's marker, in its first prompt
	prompts     int      // prompts this walk
	pqueued     string   // the parent's queued turn, not yet taken
	pheld       []string // parent requests taken and not answered
	heldAt      int      // transcript length when the newest was seen taken
	slot        [2]*scChild
	marks       []string // every foreground child's marker this walk
	fan         [2]string
	fanMark     string            // the last accepted spawnAll's block marker
	lastAll     bool              // the last accepted foreground spawn was spawnAll
	bgMark      string            // the background attempt in flight
	bgTurn      string            // its agent's turn
	bgTurns     map[string]string // agent id -> its held turn
	known       map[string]bool   // agent ids the model was handed
	bgAttempts  int
	did         map[string]int
	wrongGiveUp bool // TestSpawnCallBudgetCancelPathsCatchWrongAdapter: GiveUp answers instead of dropping
}

func newSCAdapter(t *testing.T) *scAdapter {
	holds := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: scConfig("loop"),
		Env:    []string{"BOUGH_TEST_CREATE_HOLD_DIR=" + holds},
	})
	a := &scAdapter{t: t, s: s, dir: control.Dir(s.Home), holds: holds, did: map[string]int{}}
	a.proxy = newSCProxy(t, s)
	return a
}

// scProxy sits between the sessions and serve (serve.pid names it), so a
// test can drop a background create both ways: the tool reads an error,
// as when serveTimeout fires, and serve sees its client gone.
type scProxy struct {
	srv   *httptest.Server
	mu    sync.Mutex
	drops map[int]context.CancelFunc
	n     int
}

func newSCProxy(t *testing.T, s *servetest.Server) *scProxy {
	u, err := url.Parse(s.URL)
	if err != nil {
		t.Fatal(err)
	}
	rp := httputil.NewSingleHostReverseProxy(u)
	rp.ErrorHandler = func(w http.ResponseWriter, r *http.Request, err error) {
		// A dropped create: the connection closes under the client.
		if hj, ok := w.(http.Hijacker); ok {
			if c, _, herr := hj.Hijack(); herr == nil {
				c.Close()
				return
			}
		}
		w.WriteHeader(http.StatusBadGateway)
	}
	p := &scProxy{drops: map[int]context.CancelFunc{}}
	p.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost && r.URL.Path == "/api/sessions" {
			ctx, cancel := context.WithCancel(r.Context())
			p.mu.Lock()
			p.n++
			n := p.n
			p.drops[n] = cancel
			p.mu.Unlock()
			defer func() {
				p.mu.Lock()
				delete(p.drops, n)
				p.mu.Unlock()
				cancel()
			}()
			rp.ServeHTTP(w, r.WithContext(ctx))
			return
		}
		rp.ServeHTTP(w, r)
	}))
	t.Cleanup(p.srv.Close)
	pf := filepath.Join(s.Home, ".bough", "serve.pid")
	b, err := os.ReadFile(pf)
	if err != nil {
		t.Fatal(err)
	}
	head, rest, _ := strings.Cut(string(b), "\t")
	f := strings.Fields(head)
	if len(f) != 2 {
		t.Fatalf("serve.pid: %q", b)
	}
	if err := os.WriteFile(pf, []byte(f[0]+" "+p.srv.Listener.Addr().String()+"\t"+rest), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// drop ends every create in flight; it returns how many there were.
func (p *scProxy) drop() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	for _, c := range p.drops {
		c()
	}
	return len(p.drops)
}

// --- the model's turns ---

func (a *scAdapter) queue(prefix string, turn control.Turn) string {
	a.q++
	name := fmt.Sprintf("%s%06d", prefix, a.q)
	control.Queue(a.t, a.dir, name, turn)
	return name
}

func (a *scAdapter) taken(name string) bool {
	_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
	return err == nil
}

// queueParent keeps one held parent turn queued.
func (a *scAdapter) queueParent() {
	if a.pqueued == "" && a.pmark != "" {
		a.pqueued = a.queue("p", control.Turn{Mode: "block", Text: "Done.", Match: a.pmark})
	}
}

// absorb moves a taken parent turn onto held and queues the next.
func (a *scAdapter) absorb() bool {
	if a.pqueued == "" || !a.taken(a.pqueued) {
		return false
	}
	a.pheld = append(a.pheld, a.pqueued)
	a.heldAt = len(a.entries())
	a.pqueued = ""
	a.queueParent()
	return true
}

// release answers the newest held parent request as turn says.
func (a *scAdapter) release(turn control.Turn) error {
	a.absorb()
	if len(a.pheld) == 0 {
		return errors.New("no parent request is held")
	}
	name := a.pheld[len(a.pheld)-1]
	a.pheld = a.pheld[:len(a.pheld)-1]
	control.ReleaseWith(a.t, a.dir, name, turn)
	return nil
}

// waitParent waits for the parent's next request to be held.
func (a *scAdapter) waitParent(what string) error {
	return a.until(what, func() bool { return a.absorb() })
}

func (a *scAdapter) queueChild(mark string) string {
	name := a.queue("c", control.Turn{Mode: "block", Text: "Status: ok", Child: true, Match: "task " + mark})
	return name
}

// until polls ok for up to actionTimeout.
func (a *scAdapter) until(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

func (a *scAdapter) exists(name string) bool {
	_, err := os.Stat(filepath.Join(a.holds, name))
	return err == nil
}

func (a *scAdapter) entries() []history.Entry {
	if a.id == "" {
		return nil
	}
	es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	return es
}

// settle waits until the transcript, the row and the llm queue have
// been still for a moment, absorbing any request the parent made.
func (a *scAdapter) settle() error {
	if a.id == "" {
		return nil
	}
	const quiet = 300 * time.Millisecond
	deadline := time.Now().Add(actionTimeout)
	last, lastAt := "", time.Now()
	for time.Now().Before(deadline) {
		if a.absorb() {
			lastAt = time.Now()
		}
		ctx, cancel := actionCtx()
		row, lines, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		ents, _ := os.ReadDir(a.dir)
		sig := fmt.Sprintf("%s %v %d %d %d", row.Status, row.Live, len(lines), len(a.pheld), len(ents))
		if sig != last {
			last, lastAt = sig, time.Now()
		} else if time.Since(lastAt) >= quiet {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("the session did not settle in %s", actionTimeout)
}

// --- walks ---

func (a *scAdapter) Init() error {
	a.walk++
	a.mode, a.id, a.pmark = "", "", ""
	a.prompts, a.n = 0, 0
	a.pqueued, a.pheld = "", nil
	a.slot = [2]*scChild{}
	a.marks = nil
	a.fan, a.fanMark, a.lastAll = [2]string{}, "", false
	a.bgMark, a.bgTurn = "", ""
	a.bgTurns, a.known = map[string]string{}, map[string]bool{}
	a.bgAttempts = 0
	a.gate.reset()
	return nil
}

// Cleanup ends the walk's sessions (archive kills a parent and its
// agents) and takes back every turn it left queued or held.
func (a *scAdapter) Cleanup() error {
	for _, h := range []string{"create.hold", "respond.hold"} {
		os.Remove(filepath.Join(a.holds, h))
	}
	a.proxy.drop()
	var errs []error
	ctx, cancel := actionCtx()
	defer cancel()
	agents := map[string]bool{}
	if a.id != "" {
		rows, _ := a.children()
		for _, r := range rows {
			agents[r.ID] = true
		}
		for id := range a.bgTurns {
			agents[id] = true
		}
	}
	for id := range agents {
		if _, err := a.s.Archive(ctx, id); err != nil {
			var apiErr *servetest.APIError
			if !errors.As(err, &apiErr) || apiErr.Status != http.StatusNotFound {
				errs = append(errs, err)
			}
		}
	}
	if a.id != "" {
		if _, err := a.s.Archive(ctx, a.id); err != nil {
			errs = append(errs, err)
		} else if _, err := waitRow(a.s, a.id, "the parent to be gone", func(r serve.Row) bool { return !r.Live }); err != nil {
			errs = append(errs, err)
		}
	}
	// Every turn this walk queued: taken ones are released (a process
	// that is gone never reads it), untaken ones removed.
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		if strings.HasSuffix(e.Name(), ".json") {
			os.Remove(filepath.Join(a.dir, e.Name()))
			continue
		}
		if name, ok := strings.CutSuffix(e.Name(), ".taken"); ok {
			if _, err := os.Stat(filepath.Join(a.dir, name+".release")); err != nil {
				control.Release(a.t, a.dir, name)
			}
		}
	}
	a.id = ""
	return errors.Join(errs...)
}

func (a *scAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Parent", Index: 0}: a}, nil
}

// --- reading the server ---

var scAgentNotice = regexp.MustCompile(`\[agent [^\]]*? · (\S+) (?:finished|stopped|failed)\]`)

// scView is one read of the server.
type scView struct {
	row     serve.Row
	entries []history.Entry
	agents  []serve.Row
	st      map[string]any
}

func scEntryText(e history.Entry) string {
	b, _ := json.Marshal(e.Data)
	return history.EntryText(e) + " " + string(b)
}

func (a *scAdapter) view() (scView, error) {
	v := scView{}
	st := map[string]any{
		"turn": "idle", "engine": a.mode, "spawns": 0, "used": 0,
		"c1": "none", "c2": "none", "fan": "none", "results": []any{"", ""}, "grand": 0,
		"bg_req": "idle", "bg_attempts": 0, "bg_told": "none", "bg_running": 0,
		"bg_known": 0, "bg_orphan_notices": 0,
	}
	v.st = st
	if a.id == "" {
		return v, nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return v, err
	}
	v.row = row
	if row.Status == serve.StatusRunning || row.Status == serve.StatusNeedsYou {
		st["turn"] = "running"
	}
	v.entries = a.entries()
	agents, err := a.children()
	if err != nil {
		return v, err
	}
	v.agents = agents

	// Foreground children.
	kids := scKids(v.entries)
	jobs := map[string]bool{}
	lastClose := -1
	grand := 0
	for i, e := range v.entries {
		switch e.Kind {
		case "sub:start":
			if strings.Contains(str(e.Data["text"]), "grand ") {
				grand++
			}
		case "job":
			if c := str(e.Data["call"]); c != "" {
				jobs[c] = str(e.Data["event"]) != "finished"
			}
		case "done", "cancelled":
			lastClose = i
		}
	}
	st["grand"] = grand
	slotState := func(c *scChild) string {
		if c == nil {
			return "none"
		}
		k, ok := kids[c.mark]
		if !ok || k.ended {
			return "none"
		}
		if jobs["spawn_"+c.mark] {
			return "adopted"
		}
		return "running"
	}
	live := 0
	for i, c := range a.slot {
		s := slotState(c)
		st[fmt.Sprintf("c%d", i+1)] = s
		if s != "none" {
			live++
		}
	}
	used := 0
	for _, k := range kids {
		if k.ended && k.at > lastClose && (k.status == "ok" || k.status == "failed") {
			used++
		}
	}
	st["used"], st["spawns"] = used, live+used

	// The last accepted spawnAll.
	if a.fanMark != "" {
		results := []any{"", ""}
		returned, cancelled := false, false
		started := false
		for _, e := range v.entries {
			switch {
			case e.Kind == "code" && strings.Contains(str(e.Data["text"]), a.fanMark):
				started = true
			case started && e.Kind == "result" && strings.Contains(str(e.Data["code"]), a.fanMark):
				returned = true
				var reports []string
				if json.Unmarshal([]byte(str(e.Data["text"])), &reports) == nil {
					for i := range min(len(reports), 2) {
						results[i] = reportToken(reports[i], a.fan)
					}
				} else {
					results = []any{"?", "?"}
				}
			case started && !returned && e.Kind == "cancelled":
				cancelled = true
			}
		}
		if !returned {
			for i, m := range a.fan {
				if k, ok := kids[m]; ok {
					switch k.status {
					case "ok", "failed":
						results[i] = fmt.Sprintf("r%d", i+1)
					case "error":
						results[i] = fmt.Sprintf("f%d", i+1)
					}
				}
			}
		}
		st["results"] = results
		if a.lastAll {
			switch {
			case returned:
				st["fan"] = "returned"
			case cancelled:
				st["fan"] = "cancelled"
			default:
				st["fan"] = "running"
			}
		}
	}

	// The background half.
	switch {
	case a.exists("create.held"):
		st["bg_req"] = "in_flight"
	case a.exists("respond.held"):
		st["bg_req"] = "created"
	}
	st["bg_attempts"], st["bg_told"] = a.bgAttempts, a.bgAnswer(v.entries)
	running, known := 0, 0
	for _, r := range agents {
		switch r.Status {
		case serve.StatusRunning, serve.StatusQueued, serve.StatusNeedsYou:
			running++
			if a.known[r.ID] {
				known++
			}
		}
	}
	st["bg_running"], st["bg_known"] = running, known
	orphans := map[string]bool{}
	for _, e := range v.entries {
		for _, m := range scAgentNotice.FindAllStringSubmatch(scEntryText(e), -1) {
			if !a.known[m[1]] {
				orphans[m[1]] = true
			}
		}
	}
	st["bg_orphan_notices"] = len(orphans)
	return v, nil
}

// scKid is one foreground child as the transcript tells it.
type scKid struct {
	ended  bool
	status string // its sub:done's
	at     int    // the sub:done's position
}

// scKids reads the foreground children off a transcript, by the marker
// in their task. A worker number names the child of its latest
// sub:start (a respawned process numbers from 1 again), and the first
// sub:done after it ends that child.
func scKids(entries []history.Entry) map[string]*scKid {
	kids := map[string]*scKid{}
	cur := map[int]string{}
	for i, e := range entries {
		w := scInt(e.Data["worker"])
		switch e.Kind {
		case "sub:start":
			m := scMark.FindString(str(e.Data["text"]))
			if !strings.HasPrefix(str(e.Data["text"]), "task ") {
				m = "" // a grandchild's
			}
			cur[w] = m
			if m != "" {
				kids[m] = &scKid{}
			}
		case "sub:done":
			if k := kids[cur[w]]; k != nil && !k.ended {
				k.ended, k.status, k.at = true, str(e.Data["status"]), i
			}
			delete(cur, w)
		}
	}
	return kids
}

var scMark = regexp.MustCompile(`c[12]-w\d+-\d+`)

// reportToken names one spawnAll report by what it says: "r1"/"r2" for a
// child's report, "f1"/"f2" for a failed child (by its task's marker).
func reportToken(r string, fan [2]string) string {
	for i, m := range fan {
		switch {
		case strings.Contains(r, fmt.Sprintf("Findings: r%d ", i+1)):
			return fmt.Sprintf("r%d", i+1)
		case m != "" && strings.Contains(r, "task "+m+" ") && strings.Contains(r, "Status: failed"):
			return fmt.Sprintf("f%d", i+1)
		}
	}
	return "?"
}

// bgAnswer reads what the model's last background attempt answered off
// the transcript ("none" before any answer): ok with an agent id, or
// error (a thrown create, a failed call, or a turn cancelled while it
// waited). How many attempts it made is the adapter's own count: they
// are the model's moves.
func (a *scAdapter) bgAnswer(entries []history.Entry) string {
	told := "none"
	open := ""
	for _, e := range entries {
		t := scEntryText(e)
		m := scBgMark.FindString(t)
		switch {
		case e.Kind == "code" && m != "":
			open = m
		case e.Kind == "result" && m != "" && strings.Contains(str(e.Data["code"]), m):
			told, open = bgTold(str(e.Data["text"])), ""
		case e.Kind == "call" && strings.HasPrefix(str(e.Data["id"]), "bash_bg-"):
			// On the engine the create's own row lands only at its end;
			// the bash call beside it marks when it was made.
			open = m
		case e.Kind == "call" && strings.HasPrefix(str(e.Data["id"]), "spawnbg_"):
			if str(e.Data["error"]) != "" || e.Data["canceled"] == true {
				told = "error"
			} else {
				told = bgTold(str(e.Data["output"]))
			}
			open = ""
		case e.Kind == "cancelled" && open != "":
			told, open = "error", ""
		}
	}
	return told
}

var scBgMark = regexp.MustCompile(`bg-w\d+-\d+`)

func bgTold(text string) string {
	var r struct {
		Session string `json:"session"`
	}
	if json.Unmarshal([]byte(strings.TrimSpace(text)), &r) == nil && r.Session != "" {
		return "ok"
	}
	return "error"
}

func (a *scAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return v.st, nil
}

func (a *scAdapter) children() ([]serve.Row, error) {
	if a.id == "" {
		return nil, nil
	}
	var r struct {
		Children []serve.Row `json:"children"`
	}
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/children", nil)
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
		return nil, fmt.Errorf("children of %s answered %d", a.id, resp.StatusCode)
	}
	err = json.NewDecoder(resp.Body).Decode(&r)
	return r.Children, err
}

func scInt(v any) int {
	switch n := v.(type) {
	case float64:
		return int(n)
	case int:
		return n
	case string:
		i, _ := strconv.Atoi(n)
		return i
	}
	return 0
}

// --- actions: the person ---

func (a *scAdapter) use(mode string) error {
	if !a.gate.pass(a.mode == "") {
		return nil
	}
	a.mode = mode
	path := filepath.Join(a.s.Home, ".bough", "bough.yml")
	if err := os.WriteFile(path+".tmp", []byte(scConfig(mode)), 0o644); err != nil {
		return err
	}
	if err := os.Rename(path+".tmp", path); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.walk))
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.pmark = fmt.Sprintf("parent-w%d", a.walk)
	a.queueParent()
	_, err = waitRow(a.s, a.id, "idle", func(r serve.Row) bool { return r.Status == serve.StatusIdle && r.Live })
	return err
}

func (a *scAdapter) UseLoop() error   { return a.use("loop") }
func (a *scAdapter) UseEngine() error { return a.use("unreal") }

func (a *scAdapter) Prompt() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.mode != "" && v.st["turn"] == "idle") {
		return nil
	}
	a.prompts++
	text := fmt.Sprintf("go %s t%d", a.pmark, a.prompts)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	for tries := 0; ; tries++ {
		if err := a.waitParent("the prompt's model request"); err != nil {
			return err
		}
		if i := a.inputAt(text); i >= 0 && i < a.heldAt {
			break
		}
		if tries == 3 {
			return errors.New("the prompt never reached a model request")
		}
		// A report stored while the process was gone (Esc ends it) wakes
		// the resumed parent before the prompt lands, and a prompt that
		// lands during that request is a steer that drops the reply's
		// blocks: the wake's request is answered, and the prompt's turn
		// (or the steer's step) asks next.
		if err := a.release(control.Turn{Text: "Noted."}); err != nil {
			return err
		}
	}
	if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	return a.settle()
}

// Esc is serve's interrupt: the turn closes as stopped and the process
// exits, so every request it held (the parent's, its children's) goes
// with it.
func (a *scAdapter) Esc() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["turn"] == "running") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the turn to stop", func(r serve.Row) bool {
		return r.Status != serve.StatusRunning && r.Status != serve.StatusNeedsYou
	}); err != nil {
		return err
	}
	a.pheld = nil
	a.slot = [2]*scChild{}
	a.afterBg(v)
	return a.settle()
}

// afterBg waits out a background create the turn's end or a drop gave
// up: serve lets go of its hold once it sees the client gone.
func (a *scAdapter) afterBg(v scView) {
	if v.st["bg_req"] == "idle" {
		return
	}
	a.until("serve to let the create go", func() bool { return !a.exists("create.held") && !a.exists("respond.held") })
	for _, h := range []string{"create.hold", "respond.hold"} {
		os.Remove(filepath.Join(a.holds, h))
	}
	// A create given up before serve made the agent may still make one
	// (the product's bug); give it the moment it takes to show.
	time.Sleep(300 * time.Millisecond)
	if a.bgTurn != "" && !a.taken(a.bgTurn) {
		os.Remove(filepath.Join(a.dir, a.bgTurn+".json"))
	}
	a.bgMark, a.bgTurn = "", ""
}

// inputAt is the position of the input entry (a prompt or a steer)
// carrying text, -1 before it is recorded.
func (a *scAdapter) inputAt(text string) int {
	for i, e := range a.entries() {
		if e.Kind == "input" && str(e.Data["text"]) == text {
			return i
		}
	}
	return -1
}

// --- actions: the model ---

// block is a code-mode reply: one js block whose failure is caught, so a
// refused spawn is a result, not an error that marks the turn.
func scBlock(js, fallback string) string {
	return "```js\ntry { " + js + " } catch (e) { '" + fallback + ": ' + e }\n```"
}

// reply releases the parent's held request with the spawn: a js block,
// or on the engine calls beside a `true` bash call.
func (a *scAdapter) reply(js string, calls []control.Call, key string) error {
	if a.mode == "loop" {
		return a.release(control.Turn{Text: js})
	}
	calls = append(calls, control.Call{ID: "bash_" + key, Name: "bash", Args: map[string]any{"command": "true"}})
	return a.release(control.Turn{Calls: calls})
}

func (a *scAdapter) nextMark(slot int) string {
	a.n++
	m := fmt.Sprintf("c%d-w%d-%d", slot+1, a.walk, a.n)
	a.marks = append(a.marks, m)
	return m
}

func scTask(m string) string { return "task " + m + " : report on it" }

// accepted waits for a spawn's outcome: its children's turns taken (it
// was accepted) or the parent asking again (refused, or the engine's
// next request). It answers whether the children started.
func (a *scAdapter) accepted(turns []string) (bool, error) {
	var ok bool
	err := a.until("the spawn to start its children or come back", func() bool {
		if a.absorb() && a.mode == "loop" {
			return true
		}
		ok = true
		for _, t := range turns {
			ok = ok && a.taken(t)
		}
		return ok
	})
	if !ok {
		for _, t := range turns {
			os.Remove(filepath.Join(a.dir, t+".json"))
		}
	}
	return ok, err
}

func (a *scAdapter) Spawn() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := v.st
	if !a.gate.pass(st["turn"] == "running" && a.mode == "loop" && st["bg_req"] == "idle" &&
		st["c1"] == "none" && st["c2"] == "none" && st["fan"] != "running") {
		return nil
	}
	m := a.nextMark(0)
	turn := a.queueChild(m)
	if err := a.reply(scBlock("tools.spawn("+strconv.Quote(scTask(m))+")", "refused"), nil, m); err != nil {
		return err
	}
	ok, err := a.accepted([]string{turn})
	if err != nil {
		return err
	}
	if ok {
		a.slot[0] = &scChild{mark: m, turn: turn}
		a.lastAll = false
	}
	return a.settle()
}

func (a *scAdapter) SpawnAll() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := v.st
	if !a.gate.pass(st["turn"] == "running" && a.mode == "loop" && st["bg_req"] == "idle" &&
		st["c1"] == "none" && st["c2"] == "none" && st["fan"] != "running") {
		return nil
	}
	m1, m2 := a.nextMark(0), a.nextMark(1)
	t1, t2 := a.queueChild(m1), a.queueChild(m2)
	key := fmt.Sprintf("all-w%d-%d", a.walk, a.n)
	js := "JSON.stringify(tools.spawnAll([" + strconv.Quote(scTask(m1)) + ", " + strconv.Quote(scTask(m2)) + "])) /* " + key + " */"
	if err := a.reply(scBlock(js, "refused"), nil, key); err != nil {
		return err
	}
	ok, err := a.accepted([]string{t1, t2})
	if err != nil {
		return err
	}
	if ok {
		a.slot[0], a.slot[1] = &scChild{mark: m1, turn: t1}, &scChild{mark: m2, turn: t2}
		a.fan, a.fanMark, a.lastAll = [2]string{m1, m2}, key, true
	}
	return a.settle()
}

func (a *scAdapter) NativeSpawn() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := v.st
	if !a.gate.pass(st["turn"] == "running" && a.mode == "unreal" && (st["c1"] == "none" || st["c2"] == "none")) {
		return nil
	}
	i := 0
	if st["c1"] != "none" {
		i = 1
	}
	m := a.nextMark(i)
	turn := a.queueChild(m)
	call := control.Call{ID: "spawn_" + m, Name: "spawn", Args: map[string]any{"task": scTask(m)}}
	if err := a.reply("", []control.Call{call}, m); err != nil {
		return err
	}
	// The spawn starts its child or fails at once; the bash call's end
	// makes the parent's next request either way.
	started := false
	err = a.until("the native spawn to start its child or fail", func() bool {
		if a.taken(turn) {
			started = true
			return true
		}
		for _, e := range a.entries() {
			if e.Kind == "call" && str(e.Data["id"]) == "spawn_"+m {
				return true
			}
		}
		return false
	})
	if err != nil {
		return err
	}
	if started {
		a.slot[i] = &scChild{mark: m, turn: turn}
	} else {
		os.Remove(filepath.Join(a.dir, turn+".json"))
	}
	if err := a.waitParent("the parent's next request"); err != nil {
		return err
	}
	return a.settle()
}

// pick is the child an action on "c1 or c2" works on: c1 first.
func (a *scAdapter) pick(st map[string]any) int {
	if st["c1"] != "none" {
		return 0
	}
	return 1
}

func (a *scAdapter) ChildSpawns() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := v.st
	if !a.gate.pass(st["c1"] != "none" || st["c2"] != "none") {
		return nil
	}
	c := a.slot[a.pick(st)]
	next := a.queueChild(c.mark)
	gtask := "grand " + c.mark + " : one level down"
	if a.mode == "loop" {
		control.ReleaseWith(a.t, a.dir, c.turn, control.Turn{Text: scBlock("tools.spawn("+strconv.Quote(gtask)+")", "refused")})
	} else {
		control.ReleaseWith(a.t, a.dir, c.turn, control.Turn{Calls: []control.Call{{ID: fmt.Sprintf("gspawn_%s_%d", c.mark, a.q), Name: "spawn", Args: map[string]any{"task": gtask}}}})
	}
	c.turn = next
	if err := a.until("the child's next request", func() bool { return a.taken(next) }); err != nil {
		return err
	}
	return a.settle()
}

// childEnds releases slot i's child as turn says and waits for what its
// end sets going: the parent's block coming back (code mode, when no
// other child holds it), or a wake turn (an adopted child).
func (a *scAdapter) childEnds(i int, turn control.Turn, v scView) error {
	c := a.slot[i]
	control.ReleaseWith(a.t, a.dir, c.turn, turn)
	a.slot[i] = nil
	if err := a.until("the child's sub:done", func() bool {
		k := scKids(a.entries())[c.mark]
		return k != nil && k.ended
	}); err != nil {
		return err
	}
	other := a.slot[1-i] != nil && v.st[fmt.Sprintf("c%d", 2-i)] != "none"
	switch {
	case a.mode == "loop" && !other:
		if err := a.waitParent("the parent's block to come back"); err != nil {
			return err
		}
	case a.mode == "unreal" && v.st["turn"] == "idle":
		if err := a.waitParent("the wake turn's request"); err != nil {
			return err
		}
	}
	return a.settle()
}

func (a *scAdapter) childDone(i int) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st[fmt.Sprintf("c%d", i+1)] != "none") {
		return nil
	}
	m := a.slot[i].mark
	return a.childEnds(i, control.Turn{Text: fmt.Sprintf("Status: ok\nFindings: r%d %s\nFiles: none\nOpen: none", i+1, m)}, v)
}

func (a *scAdapter) Child1Done() error { return a.childDone(0) }
func (a *scAdapter) Child2Done() error { return a.childDone(1) }

func (a *scAdapter) Child1LLMFails() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["c1"] != "none") {
		return nil
	}
	return a.childEnds(0, control.Turn{Mode: "error", Error: "provider unavailable"}, v)
}

// Done lets the turn close: every request the parent makes is answered
// with text until it does.
func (a *scAdapter) Done() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := v.st
	if !a.gate.pass(st["turn"] == "running" && st["fan"] != "running" && st["bg_req"] == "idle" &&
		(a.mode == "unreal" || (st["c1"] == "none" && st["c2"] == "none"))) {
		return nil
	}
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		a.absorb()
		for len(a.pheld) > 0 {
			if err := a.release(control.Turn{Text: "Done."}); err != nil {
				return err
			}
		}
		ctx, cancel := actionCtx()
		row, _, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		if row.Status != serve.StatusRunning && row.Status != serve.StatusNeedsYou {
			return a.settle()
		}
		time.Sleep(20 * time.Millisecond)
	}
	return errors.New("done: the turn did not close")
}

// --- actions: background agents ---

func (a *scAdapter) SpawnBg() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	st := v.st
	if !a.gate.pass(st["turn"] == "running" && st["bg_req"] == "idle" && scInt(st["bg_attempts"]) < scMaxBgAttempts &&
		(a.mode == "unreal" || (st["c1"] == "none" && st["c2"] == "none"))) {
		return nil
	}
	a.bgAttempts++
	a.bgMark = fmt.Sprintf("bg-w%d-%d", a.walk, a.bgAttempts)
	for _, h := range []string{"create.hold", "respond.hold"} {
		if err := os.WriteFile(filepath.Join(a.holds, h), nil, 0o644); err != nil {
			return err
		}
	}
	a.bgTurn = a.queue("b", control.Turn{Mode: "block", Text: "agent done", Match: a.bgMark})
	btask := a.bgMark + " : work in the background"
	js := scBlock("JSON.stringify(tools.spawn("+strconv.Quote(btask)+", {background: true}))", "bg-error")
	call := control.Call{ID: "spawnbg_" + a.bgMark, Name: "spawn", Args: map[string]any{"task": btask, "background": true}}
	if err := a.reply(js, []control.Call{call}, a.bgMark); err != nil {
		return err
	}
	if err := a.until("serve to hold the create", func() bool { return a.exists("create.held") }); err != nil {
		return err
	}
	if a.mode == "unreal" {
		if err := a.waitParent("the parent's next request"); err != nil {
			return err
		}
	}
	return a.settle()
}

func (a *scAdapter) ServerCreates() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["bg_req"] == "in_flight") {
		return nil
	}
	os.Remove(filepath.Join(a.holds, "create.hold"))
	if err := a.until("serve to make the agent", func() bool { return a.exists("respond.held") }); err != nil {
		return err
	}
	id := a.heldID()
	a.bgTurns[id] = a.bgTurn
	if err := a.until("the agent's model request", func() bool { return a.taken(a.bgTurn) }); err != nil {
		return err
	}
	if _, err := a.waitAgent(id, "running", func(r *serve.Row) bool { return r != nil && r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	return a.settle()
}

func (a *scAdapter) Respond() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["bg_req"] == "created") {
		return nil
	}
	id := a.heldID()
	os.Remove(filepath.Join(a.holds, "respond.hold"))
	if err := a.until("the tool to read the agent's id", func() bool {
		a.absorb()
		return !a.exists("respond.held") && a.lastBgAnswered() && (a.mode == "unreal" || len(a.pheld) > 0)
	}); err != nil {
		return err
	}
	a.known[id] = true
	a.bgMark, a.bgTurn = "", ""
	return a.settle()
}

// lastBgAnswered says whether the attempt in flight has its answer in the
// transcript: its block's result, or its call's end.
func (a *scAdapter) lastBgAnswered() bool {
	for _, e := range a.entries() {
		switch {
		case e.Kind == "result" && strings.Contains(str(e.Data["code"]), a.bgMark):
			return true
		case e.Kind == "call" && str(e.Data["id"]) == "spawnbg_"+a.bgMark:
			return true
		}
	}
	return false
}

func (a *scAdapter) GiveUp() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["bg_req"] == "in_flight" || v.st["bg_req"] == "created") {
		return nil
	}
	if a.wrongGiveUp {
		// The wrong adapter lets the create through instead.
		os.Remove(filepath.Join(a.holds, "create.hold"))
		os.Remove(filepath.Join(a.holds, "respond.hold"))
	} else if a.proxy.drop() == 0 {
		return errors.New("give up: no create in flight at the proxy")
	}
	if err := a.until("the tool to read its answer", func() bool {
		a.absorb()
		return a.lastBgAnswered() && (a.mode == "unreal" || len(a.pheld) > 0)
	}); err != nil {
		return err
	}
	a.afterBg(v)
	return a.settle()
}

func (a *scAdapter) BgReport() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(scInt(v.st["bg_known"]) > 0) {
		return nil
	}
	var id string
	for _, r := range v.agents {
		if a.known[r.ID] && r.Status == serve.StatusRunning {
			id = r.ID
			break
		}
	}
	return a.agentEnds(id, v)
}

func (a *scAdapter) BgOrphanReports() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	pending := 0
	if v.st["bg_req"] == "created" {
		pending = 1
	}
	if !a.gate.pass(scInt(v.st["bg_running"])-scInt(v.st["bg_known"])-pending > 0) {
		return nil
	}
	var id string
	for _, r := range v.agents {
		if !a.known[r.ID] && r.Status == serve.StatusRunning && a.bgTurns[r.ID] != "" && a.bgTurns[r.ID] != a.bgTurn {
			id = r.ID
			break
		}
	}
	if id == "" {
		return errors.New("no orphan agent to finish")
	}
	return a.agentEnds(id, v)
}

// agentEnds finishes a background agent's turn and waits for its report
// to reach the parent. A parent between turns takes the report as a wake
// turn, which the adapter answers at once: the spec has no such turn.
func (a *scAdapter) agentEnds(id string, v scView) error {
	turn := a.bgTurns[id]
	if turn == "" {
		return fmt.Errorf("no held turn for agent %s", id)
	}
	control.Release(a.t, a.dir, turn)
	delete(a.bgTurns, id)
	if _, err := a.waitAgent(id, "its turn to close", func(r *serve.Row) bool { return r == nil || r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	// A parent mid-turn reads the report at its next step; a live one
	// between turns wakes for it; one whose process is gone (Esc ends it)
	// finds it stored in its file.
	if v.st["turn"] == "idle" {
		woke := false
		if err := a.until("the report's wake turn or its stored notice", func() bool {
			if a.absorb() {
				woke = true
				return true
			}
			for _, e := range a.entries() {
				if e.Kind == "notice" && e.Data["from"] == id {
					return true
				}
			}
			return false
		}); err != nil {
			return err
		}
		if !woke {
			return a.settle()
		}
		if err := a.release(control.Turn{Text: "Noted."}); err != nil {
			return err
		}
		if _, err := waitRow(a.s, a.id, "the wake turn to close", func(r serve.Row) bool {
			return r.Status != serve.StatusRunning && r.Status != serve.StatusNeedsYou
		}); err != nil {
			return err
		}
	}
	return a.settle()
}

// heldID is the agent serve holds the answer for (respond.held names
// it, once it is written).
func (a *scAdapter) heldID() string {
	id := ""
	a.until("serve to name the agent it made", func() bool {
		b, _ := os.ReadFile(filepath.Join(a.holds, "respond.held"))
		id = strings.TrimSpace(string(b))
		return id != ""
	})
	return id
}

func (a *scAdapter) waitAgent(id, what string, ok func(*serve.Row) bool) (*serve.Row, error) {
	deadline := time.Now().Add(actionTimeout)
	var last *serve.Row
	for {
		rows, err := a.children()
		if err == nil {
			last = nil
			for i := range rows {
				if rows[i].ID == id {
					last = &rows[i]
				}
			}
			if ok(last) {
				return last, nil
			}
		}
		if time.Now().After(deadline) {
			b, _ := json.Marshal(last)
			return last, fmt.Errorf("waiting for %s of agent %s: last row %s, last error %v", what, id, b, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func scAction(name string, f func(*scAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*scAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var scActions = map[string]map[string]fmbt.ActionFunc{"Parent": {
	"UseLoop":         scAction("UseLoop", (*scAdapter).UseLoop),
	"UseEngine":       scAction("UseEngine", (*scAdapter).UseEngine),
	"Prompt":          scAction("Prompt", (*scAdapter).Prompt),
	"Spawn":           scAction("Spawn", (*scAdapter).Spawn),
	"SpawnAll":        scAction("SpawnAll", (*scAdapter).SpawnAll),
	"NativeSpawn":     scAction("NativeSpawn", (*scAdapter).NativeSpawn),
	"ChildSpawns":     scAction("ChildSpawns", (*scAdapter).ChildSpawns),
	"Child1Done":      scAction("Child1Done", (*scAdapter).Child1Done),
	"Child2Done":      scAction("Child2Done", (*scAdapter).Child2Done),
	"Child1LLMFails":  scAction("Child1LLMFails", (*scAdapter).Child1LLMFails),
	"Esc":             scAction("Esc", (*scAdapter).Esc),
	"Done":            scAction("Done", (*scAdapter).Done),
	"SpawnBg":         scAction("SpawnBg", (*scAdapter).SpawnBg),
	"ServerCreates":   scAction("ServerCreates", (*scAdapter).ServerCreates),
	"Respond":         scAction("Respond", (*scAdapter).Respond),
	"GiveUp":          scAction("GiveUp", (*scAdapter).GiveUp),
	"BgReport":        scAction("BgReport", (*scAdapter).BgReport),
	"BgOrphanReports": scAction("BgOrphanReports", (*scAdapter).BgOrphanReports),
}}

func scOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 14, "max-parallel-runs": 0}
}

// spawnCallBudgetCancelHistory reads the parent's turns off its
// transcript: the engine it ran on (an "engine" entry), each prompt, the
// turn's close (done, or cancelled for Esc) and the foreground spawns it
// made. Children's ends and background agents are left to the walk: a
// child's end and its parent's next request interleave in ways the
// transcript does not order, so the trace ends at the first turn that
// spawned anything; before that it checks the turn lifecycle.
func spawnCallBudgetCancelHistory(entries []history.Entry) []tracecheck.Step {
	turn := func(t string) map[string]any { return map[string]any{"Parent#0.turn": t} }
	steps := []tracecheck.Step{{Action: "Init", State: turn("idle")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Parent#0." + action, State: state})
	}
	use := "UseLoop"
	for _, e := range entries {
		if e.Kind == "engine" {
			use = "UseEngine"
		}
	}
	add(use, turn("idle"))
	open, lastClose := false, ""
	for _, e := range entries {
		switch e.Kind {
		case "input":
			wake, _ := e.Data["wake"].(bool)
			steer, _ := e.Data["steer"].(bool)
			if wake || steer {
				return steps
			}
			open, lastClose = true, ""
			add("Prompt", turn("running"))
		case "code", "call", "sub:start", "job", "notice":
			return steps
		case "cancelled":
			if open {
				add("Esc", turn("idle"))
			}
			open, lastClose = false, "cancelled"
		case "done":
			if !open && lastClose == "cancelled" {
				continue
			}
			if open {
				add("Done", turn("idle"))
			}
			open, lastClose = false, "done"
		}
	}
	return steps
}

func init() { historyProjections["spawn_call_budget_cancel"] = spawnCallBudgetCancelHistory }

// The projection has to be able to fail: a turn that never closed before
// the next prompt, or a close with no turn open, is no path in the model.
func TestSpawnCallBudgetCancelHistoryRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("spawn_call_budget_cancel")), "..", "testdata", "spawn_call_budget_cancel"))
	if err != nil {
		t.Fatal(err)
	}
	in := history.Entry{Kind: "input", Data: map[string]any{"text": "go"}}
	done := history.Entry{Kind: "done"}
	stop := history.Entry{Kind: "cancelled"}
	if v := g.Check(spawnCallBudgetCancelHistory([]history.Entry{{Kind: "engine"}, in, done, in, stop, done})); v != nil {
		t.Fatalf("a done turn, then a stopped one: %v", v)
	}
	if v := g.Check(spawnCallBudgetCancelHistory([]history.Entry{in, done})); v != nil {
		t.Fatalf("one turn on the loop: %v", v)
	}
	if g.Check(spawnCallBudgetCancelHistory([]history.Entry{in, in})) == nil {
		t.Error("a prompt into an open turn was accepted as a path in the model")
	}
}

func TestSpawnCallBudgetCancel(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSCAdapter(t)
	if err := runMBT(t, "spawn_call_budget_cancel", a, scActions, scOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g, err := tracecheck.Load(fizzCheck(t, "spawn_call_budget_cancel"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), spawnCallBudgetCancelHistory)
	}
}

func scPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("spawn_call_budget_cancel", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range f.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// walkSCPath drives one generated path and compares the whole state
// after every step; the first difference ends the path.
func walkSCPath(a *scAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int, want map[string]any) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		gb, _ := json.Marshal(got)
		var g map[string]any
		json.Unmarshal(gb, &g)
		var bad []string
		for k, v := range want {
			field, ok := strings.CutPrefix(k, "Parent#0.")
			if !ok {
				continue
			}
			wb, _ := json.Marshal(v)
			hb, _ := json.Marshal(g[field])
			if string(wb) != string(hb) {
				bad = append(bad, fmt.Sprintf("%s = %s, want %s", field, hb, wb))
			}
		}
		if len(bad) > 0 {
			slices.Sort(bad)
			return fmt.Errorf("step %d (%s): %s\n  got  %s\n  transcript tail:\n%s", i, path[i].Action, strings.Join(bad, "; "), gb, a.tail(20))
		}
		return nil
	}
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	if err := check(0, path[0].State); err != nil {
		return err
	}
	for i := 1; i < len(path); i++ {
		name := strings.TrimPrefix(path[i].Action, "Parent#0.")
		fn, ok := scActions["Parent"][name]
		if !ok {
			return fmt.Errorf("step %d: no adapter action %q", i, name)
		}
		if _, err := fn(a, nil); err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", i, name)
		}
		if err := check(i, path[i].State); err != nil {
			return err
		}
	}
	return nil
}

// tail is the parent transcript's last n entries, one line each: what a
// failed step's message shows of the product's side.
func (a *scAdapter) tail(n int) string {
	es := a.entries()
	es = es[max(0, len(es)-n):]
	var b strings.Builder
	for _, e := range es {
		t := scEntryText(e)
		if r := []rune(t); len(r) > 160 {
			t = string(r[:160]) + "…"
		}
		fmt.Fprintf(&b, "    %s: %s\n", e.Kind, strings.ReplaceAll(t, "\n", " "))
	}
	return b.String()
}

func scActs(path []tracecheck.Step) []string {
	var out []string
	for _, st := range path[1:] {
		out = append(out, strings.TrimPrefix(st.Action, "Parent#0."))
	}
	return out
}

// TestSpawnCallBudgetCancelPaths walks every derived path against real
// serves (four share them) and replays each parent transcript on the
// graph.
func TestSpawnCallBudgetCancelPaths(t *testing.T) {
	t.Parallel()
	paths := scPaths(t, envCover())
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("spawn_call_budget_cancel")), "..", "testdata", "spawn_call_budget_cancel"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newSCAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkSCPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v", i, scActs(paths[i]), err)
				}
			}
			t.Logf("steps taken: %v", a.did)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), spawnCallBudgetCancelHistory)
			}
		})
	}
}

// The path walk proves nothing unless a wrongly wired adapter fails it:
// a GiveUp that lets the create through instead must be caught on the
// first GiveUp transition a walk takes.
func TestSpawnCallBudgetCancelPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSCAdapter(t)
	a.wrongGiveUp = true
	for _, p := range scPaths(t, tracecheck.CoverTransitions) {
		cut := 0
		for i, st := range p {
			if st.Action == "Parent#0.GiveUp" {
				cut = i + 1
				break
			}
		}
		if cut == 0 {
			continue
		}
		err := walkSCPath(a, p[:cut])
		switch {
		case err == nil:
			t.Fatalf("a walk whose GiveUp answers the create passed: %v", scActs(p[:cut]))
		case !strings.Contains(err.Error(), fmt.Sprintf("step %d (Parent#0.GiveUp)", cut-1)):
			t.Fatalf("the walk failed before its GiveUp, so it says nothing about the adapter: %v: %v", scActs(p[:cut]), err)
		}
		t.Logf("caught: %v", err)
		return
	}
	t.Fatal("no transitions walk takes GiveUp")
}
