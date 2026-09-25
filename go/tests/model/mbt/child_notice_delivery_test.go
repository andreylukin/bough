//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/child_notice_delivery.fizz against a real serve: one background
// child (a real agent whose turn llm-control holds) reports to one
// parent session, and the parent's process is started, killed, resumed
// in a CLI of the adapter's own, prompted, made to ask, and has its
// tools row disabled and re-enabled (the job-notices gap) by rewriting
// the bough.yml in its cwd, which the parent hot-reloads.
//
// Most of the spec's report steps (Key, Route, WriteStdin, PumpReads,
// Poll, Wake, Land) are the product's own and take microseconds; a test
// cannot stop the real system between them. So the adapter keeps the
// report virtual (the child's turn stays held) and realizes it only when
// the spec's report reaches a state the real system also rests in: the
// file of a parent with no process, a notice hlNotice holds during a
// gap, a notice inside a turn the adapter holds, a notice appended during
// a gap, a
// delivery, a secret that took the notice. After every step the state
// read off the server (process, busy, ask, whether the report reached
// the model, whether it sits undelivered in the file, what the secret
// holds) must be the spec's, or one the spec reaches from it by the
// product's own steps alone (the real system is allowed to be ahead).
//
// Two stand-ins, where the product's own window cannot be entered from
// outside:
//   - the report's append while the parent runs (the TOCTOU the spec
//     models as Route to the file, then Append after a Mount) is written
//     by the adapter with history.AppendFile, the entry notifyFrom writes;
//     the child's report is then never released.
//   - ServeCrash (serve dying between saving Reported and notifying)
//     stays virtual: nothing is ever released, so the check is only that
//     nothing arrives.
//
// A walk that needs the real system held inside one of those microsecond
// windows (a Kill between the pump reading the notice and the wake, say)
// cannot be driven: when a step's check fails right after a check that
// found the real system ahead of the spec, the rest of the walk is
// counted as undrivable rather than failed, and the count is logged.

const (
	cndRole = "Flow#0."
	// cndInternal are the product's own steps: the real system may be
	// ahead of the spec by any number of them.
	cndInternalList = "Key Route WriteStdin Append PumpReads Poll Wake Land"
)

type cndState struct {
	report, proc, ask, prompt, lostHow, answered string
	busy, gap                                    bool
	delivered                                    int
}

func cndOf(st map[string]any) cndState {
	s := func(k string) string { v, _ := st[cndRole+k].(string); return v }
	b := func(k string) bool { v, _ := st[cndRole+k].(bool); return v }
	n := func(k string) int {
		switch v := st[cndRole+k].(type) {
		case float64:
			return int(v)
		case int:
			return v
		case int64:
			return int(v)
		}
		return 0
	}
	return cndState{
		report: s("report"), proc: s("proc"), ask: s("ask"), prompt: s("prompt"),
		lostHow: s("lost_how"), answered: s("answered_by_notice"),
		busy: b("busy"), gap: b("gap"), delivered: n("delivered"),
	}
}

// cndObs is what the adapter reads off the server for one check.
type cndObs struct {
	proc      string // none, live, cli
	busy      bool
	ask       string // none, ask, secret
	delivered int    // 1 once a model request carried the report
	unmarked  bool   // the report sits in the parent's file, not marked delivered
	answered  string // "secret" when the secret's value is the notice
}

// matches compares the fields the server shows. starting is serve's
// lease before the process exists; the adapter spawns at Mount, so the
// server shows no process then. Before the report is made real the
// server must show none of it, whatever virtual state the spec's report
// is in.
func (o cndObs) matches(s cndState, realized bool) bool {
	if !realized {
		s.delivered, s.answered, s.report = 0, "", ""
	}
	proc := s.proc
	if proc == "starting" {
		proc = "none"
	}
	answered := s.answered
	if answered == "none" {
		answered = ""
	}
	stored := s.report == "stored"
	return o.proc == proc && o.busy == s.busy && o.ask == s.ask && o.delivered == s.delivered &&
		o.unmarked == stored && o.answered == answered
}

type cndAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	keychain string
	g        *tracecheck.Graph
	reach    [][]int // per node: the nodes its internal steps reach, itself first

	walk, turn int
	parent     string
	child      string
	pcwd, ccwd string
	marker     string
	childTurn  string // the child's held turn, "" once released
	next       string // the block turn queued for the parent's next request
	held       string // the parent's request held in flight, "" when none
	startSeq   int64  // the parent file's last seq before its current process
	realized   bool
	realizedAt time.Time
	viaFile    bool // the spec's report went through the file
	viaPipe    bool // the spec's report went through the stdin pipe
	synthetic  bool // the adapter appended the report itself
	// secretAnswers counts the secrets the adapter answered itself.
	secretAnswers int
	gapOn         bool
	ev            *cndEvents
	final         []history.Entry // the parent's file at the end of a whole walk

	cli     *exec.Cmd
	cliIn   io.WriteCloser
	cliDone chan struct{}

	evCancel context.CancelFunc

	parents []string // every walk's parent
	did     map[string]int
	skipped int // walks cut short where the real system cannot be held

	// noGap is TestChildNoticeDeliveryCatchesWrongAdapter's bug:
	// ReloadStart writes nothing, so the parent keeps job-notices.
	noGap bool
}

var cndConfig = controlConfig
var cndGapConfig = controlConfig + "- id: tools\n  plugin: tools-basic\n  disabled: true\n"

func newCNDAdapter(t *testing.T, g *tracecheck.Graph) *cndAdapter {
	keychain := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files:  map[string]string{".bough/projects/" + askProject + "/project.yml": "name: " + askProject + "\n"},
		Env:    []string{"BOUGH_TEST_KEYCHAIN_DIR=" + keychain},
	})
	a := &cndAdapter{t: t, s: s, dir: control.Dir(s.Home), keychain: keychain, g: g, did: map[string]int{}}
	a.reach = cndReach(g)
	return a
}

// cndReach closes every node over the internal steps.
func cndReach(g *tracecheck.Graph) [][]int {
	internal := map[string]bool{}
	for _, n := range strings.Fields(cndInternalList) {
		internal[cndRole+n] = true
	}
	out := make([][]int, len(g.Nodes))
	adj := make([][]int, len(g.Nodes))
	for _, l := range g.Links {
		if internal[l.Name] {
			adj[l.Src] = append(adj[l.Src], l.Dest)
		}
	}
	for i := range g.Nodes {
		seen := map[int]bool{i: true}
		q := []int{i}
		for k := 0; k < len(q); k++ {
			for _, d := range adj[q[k]] {
				if !seen[d] {
					seen[d] = true
					q = append(q, d)
				}
			}
		}
		out[i] = q
	}
	return out
}

func (a *cndAdapter) hist(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

// Init writes a fresh parent with no process and starts its child,
// whose turn llm-control holds: the report is "closed" in the spec's
// sense only once the adapter releases it, which is the point.
func (a *cndAdapter) Init() error {
	a.walk++
	a.pcwd = a.s.Dir(a.t, fmt.Sprintf("p%04d", a.walk))
	a.ccwd = a.s.Dir(a.t, fmt.Sprintf("c%04d", a.walk))
	if err := os.WriteFile(filepath.Join(a.pcwd, "bough.yml"), []byte(cndConfig), 0o644); err != nil {
		return err
	}
	a.parent = history.NewID()
	path := a.hist(a.parent)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		return err
	}
	e, err := history.AppendFile(path, "meta", map[string]any{"cwd": a.pcwd})
	if err != nil {
		return err
	}
	a.startSeq = e.Seq
	a.parents = append(a.parents, a.parent)
	a.marker = fmt.Sprintf("report-w%04d-%s", a.walk, a.parent[len(a.parent)-6:])
	a.realized, a.viaFile, a.viaPipe, a.synthetic, a.gapOn = false, false, false, false, false
	a.held, a.next, a.secretAnswers = "", "", 0

	a.childTurn = fmt.Sprintf("a%05d", a.walk)
	if err := cndQueue(a.dir, a.childTurn, control.Turn{Mode: "block", Text: a.marker}); err != nil {
		return err
	}
	var r spawnReply
	status, err := a.call(http.MethodPost, "/api/sessions", map[string]any{
		"cwd": a.ccwd, "prompt": "task " + a.childTurn, "spawnedBy": a.parent,
	}, &r)
	if err != nil {
		return err
	}
	if status != http.StatusCreated {
		return fmt.Errorf("spawn answered %d", status)
	}
	a.child = r.Session.ID
	if err := waitTaken(a.dir, a.childTurn); err != nil {
		return err
	}
	a.queueNext()
	a.watchEvents()
	return nil
}

// queueNext keeps one block turn queued for the parent, so any request
// its engine makes (a wake, a prompt, a continuation) is held.
func (a *cndAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("b%06d", a.turn)
	if err := cndQueue(a.dir, a.next, control.Turn{Mode: "block", Text: "parent " + a.next}); err != nil {
		a.t.Error(err)
	}
}

func cndQueue(dir, name string, turn control.Turn) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	b, err := json.Marshal(turn)
	if err != nil {
		return err
	}
	tmp := filepath.Join(dir, name+".tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(dir, name+".json"))
}

func cndRelease(dir, name string, turn *control.Turn) error {
	var b []byte
	if turn != nil {
		var err error
		if b, err = json.Marshal(turn); err != nil {
			return err
		}
	}
	tmp := filepath.Join(dir, name+".release-tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, filepath.Join(dir, name+".release"))
}

func taken(dir, name string) bool {
	_, err := os.Stat(filepath.Join(dir, name+".taken"))
	return err == nil
}

// sync notes a request the parent's engine started: the queued turn is
// now the held one, and another is queued behind it.
func (a *cndAdapter) sync() {
	if a.next != "" && taken(a.dir, a.next) {
		a.held = a.next
		a.queueNext()
	}
}

// waitRequest waits for the parent's engine to take the queued turn.
func (a *cndAdapter) waitRequest(what string) error {
	name := a.next
	deadline := time.Now().Add(actionTimeout)
	for !taken(a.dir, name) {
		if time.Now().After(deadline) {
			return fmt.Errorf("%s: turn %s not taken after %s", what, name, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	a.sync()
	return nil
}

// cndEvents counts what the parent's event stream said during one walk.
type cndEvents struct {
	mu                        sync.Mutex
	reloads, dropped, notices int
}

// watchEvents counts the parent's stderr lines serve relays as "error"
// events ("bough: reloaded" says a config edit took, "notice dropped"
// says hlNotice gave up) and serve's own "notice" events (the report was
// written to stdin or appended).
func (a *cndAdapter) watchEvents() {
	ctx, cancel := context.WithCancel(context.Background())
	a.evCancel = cancel
	ev := &cndEvents{}
	a.ev = ev
	st, err := a.s.Events(ctx, a.parent)
	if err != nil {
		a.t.Errorf("events of %s: %v", a.parent, err)
		return
	}
	go func() {
		for {
			e, err := st.Next()
			if err != nil {
				return
			}
			ev.mu.Lock()
			switch {
			case e.Kind == "notice":
				ev.notices++
			case e.Kind != "error":
			case strings.HasPrefix(e.Text, "bough: reloaded"):
				ev.reloads++
			case strings.Contains(e.Text, "notice dropped"):
				ev.dropped++
			}
			ev.mu.Unlock()
		}
	}()
}

func (a *cndAdapter) counts() (reloads, dropped int) {
	a.ev.mu.Lock()
	defer a.ev.mu.Unlock()
	return a.ev.reloads, a.ev.dropped
}

// Cleanup ends the walk's processes: the parent and CLI are killed, the
// child's turn is released and its process ended, and the queued turn
// is taken back so the next walk's child does not answer with it.
func (a *cndAdapter) Cleanup() error {
	var errs []error
	a.killCLI()
	if err := a.killParent(); err != nil {
		errs = append(errs, err)
	}
	if a.evCancel != nil {
		a.evCancel()
	}
	if a.next != "" {
		os.Remove(filepath.Join(a.dir, a.next+".json"))
		a.next = ""
	}
	if a.childTurn != "" {
		cndRelease(a.dir, a.childTurn, nil)
		a.childTurn = ""
	}
	if a.child != "" {
		if _, err := a.waitChild("its turn to close", func(r *serve.Row) bool { return r == nil || r.Status != serve.StatusRunning }); err != nil {
			errs = append(errs, err)
		} else if err := a.endChild(); err != nil {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

func (a *cndAdapter) endChild() error {
	row, err := a.waitChild("a row", func(*serve.Row) bool { return true })
	if err != nil || row == nil || !row.Live {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.child); err != nil {
		var apiErr *servetest.APIError
		if !errors.As(err, &apiErr) || apiErr.Status != http.StatusNotFound {
			return err
		}
	}
	_, err = a.waitChild("its process to exit", func(r *serve.Row) bool { return r == nil || !r.Live })
	return err
}

func (a *cndAdapter) waitChild(what string, ok func(*serve.Row) bool) (*serve.Row, error) {
	deadline := time.Now().Add(actionTimeout)
	var last *serve.Row
	for {
		var r struct {
			Children []serve.Row `json:"children"`
		}
		_, err := a.call(http.MethodGet, "/api/sessions/"+a.parent+"/children", nil, &r)
		if err == nil {
			last = nil
			for i := range r.Children {
				if r.Children[i].ID == a.child {
					last = &r.Children[i]
				}
			}
			if ok(last) {
				return last, nil
			}
		}
		if time.Now().After(deadline) {
			b, _ := json.Marshal(last)
			return last, fmt.Errorf("waiting for %s of child %s: last row %s, last error %v", what, a.child, b, err)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// killParent SIGKILLs the serve child running the parent (a crash),
// found by the `-r <parent>` serve starts it with (the CLI the adapter
// runs carries the same, and is not serve's).
func (a *cndAdapter) killParent() error {
	out, _ := exec.Command("pgrep", "-f", "--", "-r "+a.parent).Output()
	for _, f := range strings.Fields(string(out)) {
		if pid, err := strconv.Atoi(f); err == nil && (a.cli == nil || pid != a.cli.Process.Pid) {
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
	_, err := waitRow(a.s, a.parent, "the parent's process to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

func (a *cndAdapter) killCLI() {
	if a.cli == nil {
		return
	}
	a.cli.Process.Kill()
	<-a.cliDone
	a.cli, a.cliIn = nil, nil
}

func (a *cndAdapter) cliAlive() bool {
	if a.cli == nil {
		return false
	}
	select {
	case <-a.cliDone:
		return false
	default:
		return true
	}
}

// entries is the parent's file.
func (a *cndAdapter) entries() []history.Entry {
	es, _ := history.Read(a.hist(a.parent))
	return es
}

func (a *cndAdapter) lastSeq() int64 {
	es := a.entries()
	if len(es) == 0 {
		return 0
	}
	return es[len(es)-1].Seq
}

// observe reads the Flow's fields off the server.
func (a *cndAdapter) observe() (cndObs, error) {
	var o cndObs
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.parent)
	if err != nil {
		return o, err
	}
	o.proc = "none"
	switch {
	case a.cliAlive():
		o.proc = "cli"
	case row.Live:
		o.proc = "live"
	}
	o.ask = "none"
	marked := map[string]bool{}
	var notices []history.Entry
	open := false
	for _, e := range a.entries() {
		switch e.Kind {
		case "notice":
			if strings.Contains(str(e.Data["text"]), a.marker) {
				notices = append(notices, e)
			}
		case "notice-delivered":
			marked[str(e.Data["id"])] = true
		}
		if e.Seq <= a.startSeq {
			continue
		}
		switch e.Kind {
		case "input":
			open = true
		case "done", "cancelled":
			open = false
			o.ask = "none"
		case "ask":
			o.ask = "ask"
			if s, _ := e.Data["secret"].(bool); s {
				o.ask = "secret"
			}
		case "ask/answer", "result":
			o.ask = "none"
		case "call":
			if t := str(e.Data["tool"]); t == "ask" || t == "secret" {
				o.ask = "none"
			}
		}
	}
	if o.proc == "none" {
		open, o.ask = false, "none"
	}
	o.busy = open
	for _, e := range notices {
		if !marked[str(e.Data["id"])] {
			o.unmarked = true
		}
	}
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		if !strings.HasSuffix(e.Name(), ".req") {
			continue
		}
		if b, err := os.ReadFile(filepath.Join(a.dir, e.Name())); err == nil && strings.Contains(string(b), a.marker) {
			o.delivered = 1
			break
		}
	}
	// A secret answered by a line the adapter never sent took the
	// notice. (Its value never reaches the keychain: the notice JSON has
	// characters a secret may not, so the store fails as well.)
	secrets := 0
	for _, e := range a.entries() {
		if s, _ := e.Data["secret"].(bool); s && e.Kind == "ask/answer" {
			secrets++
		}
	}
	if secrets > a.secretAnswers {
		o.answered = "secret"
	}
	return o, nil
}

// check waits for the server to show the spec's state at node n, or one
// n's internal steps reach. It reports whether the match was only ahead.
//
// With quiet set the real system may still be moving (a report was made
// real, a process started, a turn released): a match counts only when
// the server still shows the same a moment later, or a check could pass
// on the instant before an engine sends the report.
func (a *cndAdapter) check(n int, settle time.Duration, quiet bool) (ahead bool, err error) {
	deadline := time.Now().Add(settle)
	var last *cndObs
	for {
		a.sync()
		o, err := a.observe()
		if err != nil {
			return false, err
		}
		for i, m := range a.reach[n] {
			if !o.matches(cndOf(a.g.Nodes[m].State), a.realized) {
				continue
			}
			if !quiet || (last != nil && *last == o) {
				return i > 0, nil
			}
			break
		}
		if quiet {
			last = &o
			time.Sleep(200 * time.Millisecond)
		}
		if time.Now().After(deadline) {
			return false, fmt.Errorf("server shows %+v; spec %+v (nothing its own steps reach matches)%s", o, cndOf(a.g.Nodes[n].State), a.dump())
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// dump is the parent's file and the child's row, for a failure.
func (a *cndAdapter) dump() string {
	var b strings.Builder
	b.WriteString("\n  parent file:")
	for _, e := range a.entries() {
		d, _ := json.Marshal(e.Data)
		if len(d) > 160 {
			d = append(d[:160], "…"...)
		}
		fmt.Fprintf(&b, "\n    %d %s %s", e.Seq, e.Kind, d)
	}
	row, _ := a.waitChild("a row", func(*serve.Row) bool { return true })
	if row != nil {
		fmt.Fprintf(&b, "\n  child: status %s live %v", row.Status, row.Live)
	}
	return b.String()
}

// ---- realizing the report ----

// needsReal says whether the spec's report rests somewhere the real
// system also rests, so it must be made real now.
func needsReal(s cndState) bool {
	switch s.report {
	case "stored":
		// In the file of a parent with no process, or of one whose
		// poll waits for job-notices to come back.
		return s.proc == "none" || s.proc == "starting" || (s.proc == "live" && s.gap)
	case "waiting", "delivered":
		return true
	case "queued":
		return s.busy
	case "lost":
		return s.lostHow == "secret_answer"
	}
	return false
}

// realize puts the report into the real system the way the spec's path
// sent it. It returns cndErrUndrivable when the real system cannot rest
// where the spec is (e.g. a notice the spec queued before a gap began).
func (a *cndAdapter) realize(s cndState) error {
	lease := s.proc == "live"
	switch {
	case a.viaPipe:
		if !lease {
			return cndErrUndrivable
		}
		switch s.report {
		case "waiting":
			if !a.gapOn {
				return cndErrUndrivable
			}
		case "lost":
			if s.lostHow != "secret_answer" {
				return cndErrUndrivable
			}
		default:
			if a.gapOn || s.ask == "secret" {
				return cndErrUndrivable
			}
		}
		return a.releaseChild()
	case a.viaFile && lease:
		if (s.report == "stored") != a.gapOn {
			return cndErrUndrivable
		}
		return a.appendReport()
	case a.viaFile:
		return a.releaseChild()
	}
	return cndErrUndrivable
}

var cndErrUndrivable = errors.New("the real system cannot rest where the spec is")

func (a *cndAdapter) releaseChild() error {
	if a.childTurn == "" {
		return errors.New("the child's report was already released")
	}
	if err := cndRelease(a.dir, a.childTurn, nil); err != nil {
		return err
	}
	a.childTurn = ""
	a.realized, a.realizedAt = true, time.Now()
	return nil
}

// appendReport is the report serve appends after its lease check found
// no parent, landing once the parent runs (notifyFrom's TOCTOU): the
// entry notifyFrom writes, written by the adapter.
func (a *cndAdapter) appendReport() error {
	text := fmt.Sprintf("[agent %s · %s finished] %s", a.child[len(a.child)-6:], a.child, a.marker)
	if _, err := history.AppendFile(a.hist(a.parent), "notice", map[string]any{"id": history.NewID(), "to": a.parent, "text": text, "from": a.child}); err != nil {
		return err
	}
	a.synthetic = true
	a.realized, a.realizedAt = true, time.Now()
	return nil
}

// settleReal waits for what a just-realized report does first.
func (a *cndAdapter) settleReal(s cndState) error {
	switch {
	case s.report == "stored":
		return a.waitFor("the report in the parent's file", func() bool {
			o, _ := a.observe()
			return o.unmarked
		})
	case s.report == "waiting":
		// serve's write, then the pump reading it into hlNotice.
		if err := a.waitFor("serve to send the report", a.noticeSent); err != nil {
			return err
		}
		time.Sleep(300 * time.Millisecond)
	case s.report == "lost" && s.lostHow == "secret_answer":
		return a.waitFor("the secret to take the report", func() bool {
			o, _ := a.observe()
			return o.answered == "secret"
		})
	case s.report == "queued":
		// Inside a turn the engine notes the report as it takes it.
		return a.waitFor("the parent's engine to take the report", a.jobNoted)
	case s.report == "delivered":
		return a.waitFor("the report to reach the model", func() bool {
			o, _ := a.observe()
			return o.delivered == 1
		})
	}
	return nil
}

// noticeSent is serve's "notice" event for this walk's report: the
// stdin write or the append happened.
func (a *cndAdapter) noticeSent() bool {
	a.ev.mu.Lock()
	defer a.ev.mu.Unlock()
	return a.ev.notices > 0
}

// jobNoted is the engine's "job" entry for the report, written when a
// running turn takes it.
func (a *cndAdapter) jobNoted() bool {
	for _, e := range a.entries() {
		if e.Kind == "job" && strings.Contains(str(e.Data["text"]), a.marker) {
			return true
		}
	}
	return false
}

func (a *cndAdapter) waitFor(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s%s", what, a.dump())
		}
		time.Sleep(20 * time.Millisecond)
	}
	return nil
}

// ---- the actions ----

// step drives one spec transition s0 -> s1.
func (a *cndAdapter) step(action string, s0, s1 cndState) error {
	switch action {
	case "WriteStdin":
		if s1.report == "stored" {
			a.viaFile = true
		} else {
			a.viaPipe = true
		}
	case "Append":
		a.viaFile = true
	case "Key", "Route", "ServeCrash", "Ensure", "PumpReads", "Poll", "end":
		// the product's own (or virtual) steps: see realize.
	case "Mount":
		return a.mount()
	case "ResumeInCli":
		return a.resumeInCLI()
	case "Kill":
		return a.kill(s0)
	case "ReloadStart":
		return a.reload(true)
	case "ReloadEnd":
		if err := a.reload(false); err != nil {
			return err
		}
		if s0.prompt == "behind" {
			return a.waitRequest("the prompt held behind the notice")
		}
	case "NoticeTimeout":
		return a.noticeTimeout(s0)
	case "Wake":
		// Made real below if it is not yet; the wake itself is the
		// engine's.
	case "Land":
		return a.land()
	case "TurnEnds":
		return a.turnEnds()
	case "Ask":
		return a.ask(false)
	case "AskSecret":
		return a.ask(true)
	case "Answer":
		return a.answer(s0)
	case "Prompt":
		return a.prompt(s0, s1)
	default:
		return fmt.Errorf("no adapter action %q", action)
	}
	return nil
}

// mount starts serve's child for the parent: a slash command is a line
// that runs no turn, and its "command" entry says the process mounted.
func (a *cndAdapter) mount() error {
	a.startSeq = a.lastSeq()
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.parent, "/help"); err != nil {
		return err
	}
	return a.waitMounted()
}

func (a *cndAdapter) waitMounted() error {
	return a.waitFor("the parent's process to mount", func() bool {
		for _, e := range a.entries() {
			if e.Seq > a.startSeq && e.Kind == "command" {
				return true
			}
		}
		return false
	})
}

// resumeInCLI is a person running `bough -r <parent>` themselves: a
// headless process serve has no lease on, fed through its stdin.
func (a *cndAdapter) resumeInCLI() error {
	a.startSeq = a.lastSeq()
	cmd := exec.Command(a.s.Bin(), "--headless", "--json", "-r", a.parent)
	cmd.Dir = a.pcwd
	cmd.Env = cndEnv(a.s.Home, "BOUGH_TEST_KEYCHAIN_DIR="+a.keychain)
	cmd.Stdout, cmd.Stderr = io.Discard, io.Discard
	in, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		return err
	}
	a.cli, a.cliIn, a.cliDone = cmd, in, make(chan struct{})
	done := a.cliDone
	go func() { cmd.Wait(); close(done) }()
	if err := a.cliLine("/cost"); err != nil {
		return err
	}
	return a.waitMounted()
}

func (a *cndAdapter) cliLine(line string) error {
	_, err := io.WriteString(a.cliIn, line+"\n")
	return err
}

// cndEnv is servetest's child environment: HOME moved, provider keys
// dropped, the page server's port off the user's.
func cndEnv(home string, extra ...string) []string {
	drop := map[string]bool{"HOME": true, "BOUGH_WEB_ADDR": true, "BOUGH_BIN": true}
	var env []string
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if drop[k] || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		env = append(env, kv)
	}
	env = append(env, "HOME="+home, "BOUGH_WEB_ADDR=127.0.0.1:0")
	return append(env, extra...)
}

func (a *cndAdapter) kill(s0 cndState) error {
	switch s0.proc {
	case "cli":
		a.killCLI()
	case "live":
		if err := a.killParent(); err != nil {
			return err
		}
	}
	a.held = ""
	if a.gapOn {
		// The next process must start with its tools: the spec's Kill
		// ends the gap with the process.
		if err := os.WriteFile(filepath.Join(a.pcwd, "bough.yml"), []byte(cndConfig), 0o644); err != nil {
			return err
		}
		a.gapOn = false
	}
	return nil
}

// reload rewrites the parent's own bough.yml with the tools row
// disabled (gap) or back, and waits for the parent to say it reloaded.
func (a *cndAdapter) reload(gap bool) error {
	body := cndConfig
	if gap {
		if a.noGap {
			a.gapOn = true
			return nil
		}
		body = cndGapConfig
	}
	was, _ := a.counts()
	if err := os.WriteFile(filepath.Join(a.pcwd, "bough.yml"), []byte(body), 0o644); err != nil {
		return err
	}
	a.gapOn = gap
	return a.waitFor("the parent to reload its config", func() bool {
		n, _ := a.counts()
		return n > was
	})
}

func (a *cndAdapter) noticeTimeout(s0 cndState) error {
	deadline := time.Now().Add(40 * time.Second)
	for {
		if _, d := a.counts(); d > 0 {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("hlNotice never dropped the notice (realized %s ago)", time.Since(a.realizedAt).Round(time.Second))
		}
		time.Sleep(100 * time.Millisecond)
	}
	if s0.prompt == "behind" {
		return a.waitRequest("the prompt held behind the dropped notice")
	}
	return nil
}

// land lets the held request finish: the engine sends the notice it
// holds on the next request.
func (a *cndAdapter) land() error {
	if o, err := a.observe(); err != nil || o.delivered == 1 {
		// Already sent: the engine started a request for it at once.
		return err
	}
	if a.held == "" {
		// No request in flight (the turn waits on a tool): the engine
		// sends the notice on a request of its own.
		return a.waitRequest("the request the engine sends the notice on")
	}
	name := a.held
	a.held = ""
	if err := cndRelease(a.dir, name, nil); err != nil {
		return err
	}
	return a.waitRequest("the request carrying the notice")
}

// turnEnds releases the held request. A steer or an answered ask the
// turn holds is sent on a request of its own first; that one is released
// too, since the spec already counted it.
func (a *cndAdapter) turnEnds() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	deliveredBefore := o.delivered == 1
	for range 4 {
		if a.held == "" {
			return errors.New("TurnEnds with no request held")
		}
		name := a.held
		a.held = ""
		before := len(a.entries())
		if err := cndRelease(a.dir, name, nil); err != nil {
			return err
		}
		next := a.next
		closed := func() bool {
			es := a.entries()
			for _, e := range es[min(before, len(es)):] {
				if e.Kind == "done" {
					return true
				}
			}
			return false
		}
		if err := a.waitFor("the turn to close or continue", func() bool { return closed() || taken(a.dir, next) }); err != nil {
			return err
		}
		if closed() {
			return nil
		}
		a.sync()
		// A continuation that first carries the report is the engine
		// landing it (ahead of the spec); anything else is a steer's or
		// an answer's.
		if b, _ := os.ReadFile(filepath.Join(a.dir, next+".req")); !deliveredBefore && strings.Contains(string(b), a.marker) {
			return nil
		}
	}
	return errors.New("the turn kept continuing")
}

func (a *cndAdapter) ask(secret bool) error {
	if a.held == "" {
		return errors.New("Ask with no request held")
	}
	name := a.held
	a.held = ""
	turn := control.Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "Which colour?", "options": []string{"red", "blue"}}}
	if secret {
		turn = control.Turn{Mode: "call", Tool: "secret", Args: map[string]any{"name": askSecret, "question": "the API token", "project": askProject}}
	}
	if err := cndRelease(a.dir, name, &turn); err != nil {
		return err
	}
	return a.waitFor("the ask to be armed", func() bool {
		o, err := a.observe()
		return err == nil && o.ask != "none"
	})
}

func (a *cndAdapter) answer(s0 cndState) error {
	text := "red"
	if s0.ask == "secret" {
		text = "s3cret-value"
		a.secretAnswers++
	}
	if s0.proc == "cli" {
		if err := a.cliLine(text); err != nil {
			return err
		}
	} else {
		if err := a.waitFor("serve to arm the ask", func() bool {
			ctx, cancel := actionCtx()
			defer cancel()
			row, _, err := a.s.GetSession(ctx, a.parent)
			return err == nil && row.Ask != nil
		}); err != nil {
			return err
		}
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Answer(ctx, a.parent, text); err != nil {
			return err
		}
	}
	if a.held != "" {
		// A request is already in flight (the engine sent a notice
		// while the ask waited): the answer goes out after it.
		return nil
	}
	return a.waitRequest("the request after the answer")
}

func (a *cndAdapter) prompt(s0, s1 cndState) error {
	text := fmt.Sprintf("prompt %d", a.turn)
	// The real turn may already be open where the spec's is not yet (a
	// wake the engine ran at once): then the prompt is a steer too.
	o, err := a.observe()
	if err != nil {
		return err
	}
	busy := s0.busy || o.busy
	if s0.proc == "cli" {
		if err := a.cliLine(text); err != nil {
			return err
		}
	} else {
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.s.Prompt(ctx, a.parent, text); err != nil {
			return err
		}
	}
	if s1.prompt == "behind" || busy {
		// Held behind the notice, or a steer the held request carries
		// at its end: no request now.
		time.Sleep(300 * time.Millisecond)
		return nil
	}
	return a.waitRequest("the prompt's turn")
}

func (a *cndAdapter) call(method, path string, body, out any) (int, error) {
	b := &baAdapter{s: a.s}
	return b.call(method, path, body, out)
}

// ---- the walk ----

// cndVirtual are the steps that touch nothing real unless they make the
// report real.
var cndVirtual = map[string]bool{
	"Key": true, "Route": true, "WriteStdin": true, "Append": true, "ServeCrash": true,
	"Ensure": true, "PumpReads": true, "Poll": true, "Wake": true, "end": true,
}

// cndWalk drives one walk, checking after every step. It returns the
// number of steps checked and whether the walk was cut short as
// undrivable.
func (a *cndAdapter) cndWalk(w tracecheck.Walk) (checked int, cut string, err error) {
	defer func() {
		if cerr := a.Cleanup(); cerr != nil && err == nil {
			err = fmt.Errorf("cleanup: %w", cerr)
		}
	}()
	if err := a.Init(); err != nil {
		return 0, "", fmt.Errorf("Init: %w", err)
	}
	if _, err := a.check(0, 2*time.Second, false); err != nil {
		return 0, "", fmt.Errorf("Init: %w", err)
	}
	cur := 0
	ahead := false
	for i, li := range w.Links {
		l := a.g.Links[li]
		if l.Type != "action" {
			continue
		}
		name := strings.TrimPrefix(l.Name, cndRole)
		s0, s1 := cndOf(a.g.Nodes[cur].State), cndOf(a.g.Nodes[l.Dest].State)
		a.did[name]++
		if !cndVirtual[name] && !ahead {
			// The product may have taken a step of its own since the last
			// check (the 1 s stored-notice poll, say): the real system is
			// then ahead of cur before this step acts on it.
			if ah, err := a.check(cur, 0, false); err == nil && ah {
				ahead = true
			}
		}
		err := a.step(name, s0, s1)
		if err == nil && !a.realized && needsReal(s1) {
			if err = a.realize(s1); err == nil {
				err = a.settleReal(s1)
			}
		}
		if errors.Is(err, cndErrUndrivable) {
			return checked, fmt.Sprintf("step %d (%s) of %s: %v", i, name, cndActs(a.g, w), err), nil
		}
		if err != nil {
			return checked, "", fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if s1.report == "stored" && (s1.proc == "live" || s1.proc == "cli") && !s1.gap && !cndVirtual[name] {
			// A mounted parent with its services up polls its file every
			// second: let that poll run, so the check sees the report it
			// takes rather than the next step racing it.
			time.Sleep(1200 * time.Millisecond)
		}
		was := ahead
		ahead, err = a.check(l.Dest, 3*time.Second, a.realized || !cndVirtual[name])
		if err != nil {
			if was {
				return checked, fmt.Sprintf("step %d (%s) of %s after the real system ran ahead: %v", i, name, cndActs(a.g, w), err), nil
			}
			return checked, "", fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		checked++
		cur = l.Dest
	}
	// Let the real system do what it will without the adapter, then
	// look once more: a lost report must stay lost, a stored one
	// stored.
	time.Sleep(1500 * time.Millisecond)
	if _, err := a.check(cur, 3*time.Second, true); err != nil {
		if ahead {
			return checked, "end: " + err.Error(), nil
		}
		return checked, "", fmt.Errorf("after the walk settled: %w", err)
	}
	// The file as the walk left it: Cleanup kills the parent and releases
	// the child, whose report then lands in it after the walk.
	a.final = a.entries()
	return checked, "", nil
}

func loadCNDGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("child_notice_delivery")), "..", "testdata", "child_notice_delivery"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// runCNDWalks drives walks across shards serves, one after another on
// each. A failed walk goes to failed (the test fails when that is nil).
func runCNDWalks(t *testing.T, g *tracecheck.Graph, walks []tracecheck.Walk, shards int, bug func(*cndAdapter), failed func(i int, err error)) {
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			shardT := t
			var a *cndAdapter
			checked, cut, full, traced := 0, 0, 0, 0
			for i := sh; i < len(walks); i += shards {
				t.Run(fmt.Sprintf("w%04d", i), func(t *testing.T) {
					if a == nil {
						a = newCNDAdapter(shardT, g)
						if bug != nil {
							bug(a)
						}
					}
					a.t = t
					n, why, err := a.cndWalk(walks[i])
					checked += n
					if err != nil {
						if failed != nil {
							failed(i, err)
							t.Logf("walk %d %s: %v", i, cndActs(g, walks[i]), err)
							return
						}
						t.Errorf("walk %d %s: %v", i, cndActs(g, walks[i]), err)
						return
					}
					if why != "" {
						cut++
						t.Logf("walk %d cut short, undrivable: %s", i, why)
						return
					}
					full++
					if !a.synthetic && failed == nil {
						// The adapter wrote nothing into this file itself.
						// Checked here, so a failure names its walk.
						traced++
						entries := a.final
						checkHistory(t, g, entries, cndHistory)
						if t.Failed() {
							t.Logf("walk %d %s: parent file: %s", i, cndActs(g, walks[i]), cndKinds(entries))
						}
					}
				})
			}
			if a != nil {
				t.Logf("steps checked %d, walks whole %d, cut short %d; actions %v", checked, full, cut, a.did)
				if failed == nil {
					t.Logf("trace-checked %d parent transcripts", traced)
				}
			}
		})
	}
}

// cndHistory reads a parent's file as the Flow's steps, with the report
// and busy as the state after each. A process start is the slash command
// every start sends first ("/help" for serve's child, "/cost" for the
// CLI); a start while a process ran is that process's Kill, which leaves
// no entry. The report is the "notice" entry from the child (the file
// route), the engine's "job" entry (taken inside a turn) and the wake
// input that carries it; what the file cannot show (a reload, a crash)
// is left out, and the steps that remain must still be a path.
func cndHistory(entries []history.Entry) []tracecheck.Step {
	report, proc, busy := "closed", "none", false
	var steps []tracecheck.Step
	add := func(action string) {
		name := action
		if action != "Init" {
			name = cndRole + action
		}
		steps = append(steps, tracecheck.Step{Action: name, State: map[string]any{cndRole + "report": report, cndRole + "busy": busy, cndRole + "proc": proc}})
	}
	pipe := func() {
		if report == "closed" {
			report = "keyed"
			add("Key")
			report = "routed_stdin"
			add("Route")
			report = "in_pipe"
			add("WriteStdin")
			report = "queued"
			add("PumpReads")
		}
	}
	start := func(text string) {
		if proc != "none" {
			proc, busy = "none", false
			add("Kill")
		}
		if text == "/help" {
			proc = "starting"
			add("Ensure")
			proc = "live"
		} else {
			proc = "cli"
		}
		// A mount queues what the file holds.
		if report == "stored" {
			report = "queued"
		}
		if text == "/help" {
			add("Mount")
		} else {
			add("ResumeInCli")
		}
	}
	isStart := func(e history.Entry) bool {
		t := str(e.Data["text"])
		return e.Kind == "command" && (t == "/help" || t == "/cost")
	}
	turnKinds := map[string]bool{"input": true, "done": true, "cancelled": true, "ask": true, "ask/answer": true, "job": true, "call": true, "assistant": true}
	// nextStart is the start command a process's entry at i belongs to,
	// -1 when it is the running process's own: a mount that finds a stored
	// notice marks it, and may record the wake turn it runs (or that
	// turn's ask), before the start's own command entry. An entry with no
	// process is the next start's; so is one followed by a start before
	// any entry of a turn (the process that ran was killed, which leaves
	// no entry). The "cancelled" that closes the killed turn is the next
	// process's too, and so is the wake turn a mount runs.
	nextStart := func(i int) int {
		for j := i + 1; j < len(entries); j++ {
			if isStart(entries[j]) {
				return j
			}
			if w, _ := entries[j].Data["wake"].(bool); w && entries[j].Kind == "input" {
				continue
			}
			if proc != "none" && turnKinds[entries[j].Kind] {
				return -1
			}
		}
		return -1
	}
	// readBeforeSecret: the pump read the report before a secret was
	// armed (it waited out a gap, which the file cannot show) when the
	// engine takes it while that secret is still pending.
	readBeforeSecret := func(i int) bool {
		for j := i + 1; j < len(entries); j++ {
			switch entries[j].Kind {
			case "job":
				return true
			case "ask/answer", "done", "cancelled", "notice":
				// A stored notice came through the file, not the pump.
				return false
			}
		}
		return false
	}
	early := map[int]bool{}
	for i, e := range entries {
		text := str(e.Data["text"])
		if e.Kind == "notice-delivered" || e.Kind == "input" || e.Kind == "job" || e.Kind == "ask" || e.Kind == "ask/answer" || e.Kind == "cancelled" {
			if j := nextStart(i); j >= 0 && !early[j] {
				start(str(entries[j].Data["text"]))
				early[j] = true
			}
		}
		switch e.Kind {
		case "meta":
			if len(steps) == 0 {
				add("Init")
			}
		case "command":
			if isStart(e) && !early[i] {
				start(text)
			}
		case "notice":
			if str(e.Data["from"]) == "" || report != "closed" {
				continue
			}
			if proc == "live" {
				// serve appends only when it holds no lease: the parent's
				// process died.
				proc, busy = "none", false
				add("Kill")
			}
			report = "keyed"
			add("Key")
			report = "routed_file"
			add("Route")
			report = "stored"
			add("Append")
		case "notice-delivered":
			// With no process yet it is the mounting process marking what
			// it read, before its first command entry; after that entry
			// the mount already queued it. Only a mounted parent's mark of
			// a stored report is the poll.
			if proc != "none" && report == "stored" {
				report = "queued"
				add("Poll")
			}
		case "job":
			if report == "stored" {
				report = "queued"
				add("Poll")
			}
			pipe()
			if report == "queued" {
				report = "delivered"
				add("Land")
			}
		case "input":
			if w, _ := e.Data["wake"].(bool); w {
				if str(e.Data["reason"]) != "notice" {
					continue
				}
				pipe()
				report, busy = "delivered", true
				add("Wake")
				continue
			}
			busy = true
			add("Prompt")
		case "ask":
			if s, _ := e.Data["secret"].(bool); s {
				if readBeforeSecret(i) {
					pipe()
				}
				add("AskSecret")
			} else {
				add("Ask")
			}
		case "ask/answer":
			add("Answer")
		case "cancelled":
			// The killed turn, closed by the next process: its Kill is
			// already read.
			busy = false
		case "done":
			if busy {
				busy = false
				add("TurnEnds")
			}
		}
	}
	return steps
}

func init() { historyProjections["child_notice_delivery"] = cndHistory }

// cndParse is cndKinds read back: a transcript written as its kinds, the
// way a failed walk logs it, so a real file's shape can be replayed here.
func cndParse(s string) []history.Entry {
	var out []history.Entry
	for _, f := range strings.Fields(s) {
		kind, arg, _ := strings.Cut(strings.TrimSuffix(f, ")"), "(")
		data := map[string]any{}
		switch {
		case kind == "command":
			data["text"] = arg
		case kind == "notice":
			data["id"], data["from"], data["text"] = "n1", "c", "[agent c · c finished] r"
		case kind == "notice-delivered":
			data["id"] = "n1"
		case arg == "secret":
			data["secret"] = true
		case kind == "input" && arg == "wake":
			data["wake"], data["reason"] = true, "notice"
		}
		out = append(out, history.Entry{Kind: kind, Data: data})
	}
	return out
}

// Transcripts the every-transition cover wrote, as cndKinds logs them,
// must be paths: a report stored for a parent with no process and marked
// by the process that mounts (before or after its first command entry,
// with its wake turn before it too), a killed process whose turn the next
// one closes as cancelled, a CLI killed without an entry, and a notice
// the pump read in a gap before a secret was armed. A wake with no
// process is not a path.
func TestChildNoticeDeliveryHistoryProjection(t *testing.T) {
	t.Parallel()
	g := loadCNDGraph(t)
	for name, file := range map[string]string{
		"mount":      "meta notice notice-delivered command(/help) input(wake) done",
		"wake first": "meta notice notice-delivered input(wake) command(/help) done",
		"cli prompt": "meta command(/cost) input done",
		"walk 15":    "meta command(/cost) system notice origin notice-delivered command(/help) system input(wake) engine input ask ask/answer call",
		"walk 12":    "meta command(/cost) system notice origin notice-delivered input(wake) command(/help) system engine ask ask/answer call assistant hook done turn-summary title input assistant done turn-summary",
		"walk 29":    "meta command(/cost) system input engine notice cancelled origin notice-delivered input(wake) command(/help) system engine",
		"walk 56":    "meta command(/cost) system input engine ask(secret) ask/answer(secret) call notice cancelled origin notice-delivered input(wake) command(/help) system engine assistant done turn-summary turn-summary title engine",
		"walk 71":    "meta origin command(/help) system input engine notice cancelled notice-delivered input(wake) command(/cost) system engine",
		"walk 137":   "meta origin command(/help) system input engine assistant done turn-summary title notice notice-delivered input(wake) command(/help) system engine ask",
		"walk 650":   "meta origin command(/help) system input engine ask(secret) job",
		"walk 709":   "meta command(/cost) system origin command(/help) system input engine ask(secret) job",
		"walk 142":   "meta command(/cost) system input engine ask(secret) notice notice-delivered job",
	} {
		if v := g.Check(cndHistory(cndParse(file))); v != nil {
			t.Errorf("%s: %v\n  file: %s", name, v, file)
		}
	}
	if v := g.Check(cndHistory(cndParse("meta input(wake)"))); v == nil {
		t.Error("a wake with no process passed the trace check")
	}
}

// cndKinds is a transcript as its entry kinds, with the texts that tell
// a start, a wake and a secret apart.
func cndKinds(entries []history.Entry) string {
	var b strings.Builder
	for _, e := range entries {
		b.WriteString(" " + e.Kind)
		switch {
		case e.Kind == "command":
			b.WriteString("(" + str(e.Data["text"]) + ")")
		case e.Data["wake"] == true:
			b.WriteString("(wake " + str(e.Data["reason"]) + ")")
		case e.Data["secret"] == true:
			b.WriteString("(secret)")
		}
	}
	return b.String()
}

func cndActs(g *tracecheck.Graph, w tracecheck.Walk) string {
	var acts []string
	for _, li := range w.Links {
		if l := g.Links[li]; l.Type == "action" {
			acts = append(acts, strings.TrimPrefix(l.Name, cndRole))
		}
	}
	return "[" + strings.Join(acts, " ") + "]"
}

// cndSample is how many of the settled-state walks the default run
// drives: all of them (~250, each with a real parent and child, some
// waiting out hlNotice's 30 s) take a quarter of an hour, which is the
// exhaustive run's budget, not every push's.
const cndSample = 40

// TestChildNoticeDeliveryPaths walks the checked-in graph against a real
// serve: an even sample of the settled-state walks by default, every
// transition with MODEL_COVER=transitions.
func TestChildNoticeDeliveryPaths(t *testing.T) {
	t.Parallel()
	g := loadCNDGraph(t)
	raw, err := pathsJSON("child_notice_delivery")
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []tracecheck.Walk `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	walks := doc.Paths
	if envCover() != tracecheck.CoverTransitions && len(walks) > cndSample {
		walks = nil
		for i := range cndSample {
			walks = append(walks, doc.Paths[i*len(doc.Paths)/cndSample])
		}
		t.Logf("driving %d of %d settled-state walks; MODEL_COVER=transitions drives every transition", len(walks), len(doc.Paths))
	}
	runCNDWalks(t, g, walks, 4, nil, nil)
}

// A green walk proves nothing unless a wrongly wired adapter fails it:
// this one's ReloadStart writes no config, so the parent keeps
// job-notices while the spec has the gap. It shows on the transition
// into "waiting" (PumpReads during a gap), so it runs the first walks
// of the every-transition cover that take it.
func TestChildNoticeDeliveryCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	g := loadCNDGraph(t)
	raw, err := pathsJSONCover("child_notice_delivery", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []tracecheck.Walk `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	var picked []tracecheck.Walk
	for _, w := range doc.Paths {
		for _, li := range w.Links {
			l := g.Links[li]
			if l.Name == cndRole+"PumpReads" && cndOf(g.Nodes[l.Dest].State).report == "waiting" {
				picked = append(picked, w)
				break
			}
		}
		if len(picked) == 3 {
			break
		}
	}
	if len(picked) == 0 {
		t.Fatal("no walk takes PumpReads into waiting")
	}
	var mu sync.Mutex
	failures := 0
	t.Run("walks", func(t *testing.T) {
		runCNDWalks(t, g, picked, 1, func(a *cndAdapter) { a.noGap = true }, func(int, error) {
			mu.Lock()
			failures++
			mu.Unlock()
		})
	})
	if failures == 0 {
		t.Fatal("every walk passed with no real gap behind ReloadStart; the walk is not checking state")
	}
	t.Logf("%d of %d walks failed with no real gap", failures, len(picked))
}
