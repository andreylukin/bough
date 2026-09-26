//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/coordinator_restart_tools_change.fizz against a real serve: one
// web session on the default engine (engine-unreal) whose coordinator is
// restarted by a tool-set change, killed by a Run error, rebuilt, and
// refused by a broken store.
//
// Every lever is one the environment really has:
//
//   - McpReload toggles an example-wordcount row in the session's own
//     bough.yml: the child hot-reloads it, the row registers or drops
//     the wordcount tool, and the engine's tool set really changes.
//   - SwitchModel is POST /model (llm-control with another model id):
//     the llm row remounts and its dependents' tools come back as they
//     were.
//   - StoreBreaks puts a directory where the session's frozen system
//     prompt (<store>/<sid>.system.md) goes: ensureRun cannot read or
//     write it, and a coordinator already running never touches it.
//     StoreHeals puts the file back.
//   - RunExitsWithError makes the session's harness log read-only and
//     lets the held model request answer: storing the response fails,
//     Run returns "store turn ... response: ...", and the log is made
//     writable again once the error is in history.
//   - NoticeArrives is POST /notify, the line serve writes when a
//     background agent reports: a wake turn of its own.
//   - The model is llm-control. One "block" turn is always queued, so
//     every request the coordinator makes is held until the adapter
//     answers it: Record is that request being taken, Reply its release
//     with text, CallTool its release with a bash call that waits on a
//     gate file, CallEnds opening the gate.
//
// What the adapter reads off the server (history, as serve derives the
// row from it): open, dones, errs, crashed, crashes, ran, and the number
// of coordinators built (one "engine" entry each), which every GetState
// checks against the builds the spec's steps imply. The rest (inflight,
// unobserved, call, restart, changed, rebuilds, store, down, uncounted)
// is the actor's private state; the adapter keeps it from its own steps,
// and each step waits for the server evidence that it happened (a
// request taken, an engine entry, an error text, a done's running).
//
// Not every transition of the graph can be made to happen on a real
// server, because the actor does some steps by itself, at once, and
// others never without a trigger. crDrivable says which links the walks
// take and why; the others are counted and logged, never silently
// skipped.
type crAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id    string
	ids   []string
	work  string
	walks int
	n     int

	wordcount bool            // the wordcount row is in bough.yml now
	model     int             // SwitchModel alternates the llm-control model id
	queued    []string        // block turns queued and not yet taken
	takenL    []string        // taken, oldest first
	released  map[string]bool // taken turns the adapter answered
	gateF     string          // the running call's gate file
	startF    string          // the running call writes it when it starts
	builds    int             // engine entries the steps so far imply
	refusals  int             // refused builds the steps so far imply
	models    int             // model swaps this walk
	stop      string          // what the restart in progress did: built | refused | none

	// finishing: a Reply left the turn quiescent with no foreground
	// call, so evaluate closes it in the same breath (Finish is the only
	// next step, crDrivable). closesAt is the done count before the
	// reply; GetState reads the one done past it as that Finish.
	finishing bool
	closesAt  int

	// The actor's private state, kept from the adapter's own steps.
	run, down, call                        string
	inflight, unobserved, restart, changed bool
	rebuilds                               int
	store                                  string
	uncounted                              bool

	ran map[string]int

	// crashAsReply is the deliberate bug TestCoordinatorRestartTools-
	// ChangeCatchesWrongAdapter injects: RunExitsWithError answers the
	// held request normally instead of failing the store, so Run never
	// errors.
	crashAsReply bool
}

// crTurnSettle is the engine row's turn_settle in every session: short
// enough that Adopt waits seconds, not the default minute, and long
// enough that the few steps a walk takes while a foreground call runs
// finish before it.
const crTurnSettle = 4 * time.Second

func crConfig(wordcount bool) string {
	c := "- id: llm\n  plugin: llm-control\n" +
		"- id: loop\n  plugin: engine-unreal\n  config:\n    turn_settle: 4s\n"
	if wordcount {
		c += "- id: wordcount\n  plugin: example-wordcount\n"
	}
	return c
}

func newCRAdapter(t *testing.T) *crAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &crAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *crAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%06d", prefix, a.n)
}

func (a *crAdapter) on(action string, enabled bool) bool {
	if !a.gate.pass(enabled) {
		return false
	}
	if a.ran == nil {
		a.ran = map[string]int{}
	}
	a.ran[action]++
	return true
}

// Init starts each walk on a fresh session in its own directory (the
// wordcount toggle is that directory's bough.yml) in the same serve.
func (a *crAdapter) Init() error {
	a.walks++
	a.work = a.s.Dir(a.t, fmt.Sprintf("w%04d", a.walks))
	a.wordcount = false
	if err := os.WriteFile(filepath.Join(a.work, "bough.yml"), []byte(crConfig(false)), 0o644); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.work, "")
	if err != nil {
		return err
	}
	a.id, a.ids = row.ID, append(a.ids, row.ID)
	a.queued, a.takenL, a.released = nil, nil, map[string]bool{}
	a.gateF, a.startF, a.builds, a.refusals, a.models, a.stop = "", "", 0, 0, 0, ""
	a.finishing, a.closesAt = false, 0
	a.run, a.down, a.call, a.store = "none", "", "none", "ok"
	a.inflight, a.unobserved, a.restart, a.changed, a.uncounted = false, false, false, false, false
	a.rebuilds = 0
	a.gate.reset()
	a.top()
	return nil
}

// Cleanup kills the walk's child (the archive kill) so a run does not
// leave a child per walk, opens a gate the walk left shut, and drops
// the turns nobody will take.
func (a *crAdapter) Cleanup() error {
	if a.gateF != "" {
		os.WriteFile(a.gateF, nil, 0o644)
	}
	a.healStore()
	os.Chmod(a.logPath(), 0o600)
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	left, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for _, f := range left {
		os.Remove(f)
	}
	return err
}

func (a *crAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// crView is what the session's history says.
type crView struct {
	open             bool
	dones, errs      int
	closes           int // done entries in the whole transcript
	crashed          bool
	crashes, builds  int
	lastSeq          int64
	lastErr          string
	doneRunning      int
	wakeCalls, wakes int
	inputs, calls    int
	models, refusals int
}

// crObserve reads a transcript the way the spec counts it: a turn is an
// input (not a steer) up to its done; errs and crashed count the errors
// written while it was open, so an error noted with no turn open (a
// rebuild refused while idle) is nobody's.
func crObserve(es []history.Entry) crView {
	var v crView
	for _, e := range es {
		v.lastSeq = e.Seq
		switch e.Kind {
		case "input":
			if st, _ := e.Data["steer"].(bool); st {
				continue
			}
			v.open, v.dones, v.errs, v.crashed = true, 0, 0, false
			v.inputs++
			if e.Data["wake"] == true {
				v.wakes++
				if e.Data["reason"] == "call" {
					v.wakeCalls++
				}
			}
		case "done":
			v.dones++
			v.closes++
			v.open = false
			n, _ := toCount(e.Data["running"])
			v.doneRunning = n
		case "error":
			text, _ := e.Data["text"].(string)
			v.lastErr = text
			crash := strings.HasPrefix(text, "engine: ")
			if crash {
				v.crashes++
			}
			if strings.HasPrefix(text, "engine-unreal: ") {
				v.refusals++
			}
			if v.open {
				v.errs++
				v.crashed = v.crashed || crash
			}
		case "engine":
			v.builds++
		case "call":
			v.calls++
		case "model":
			v.models++
		}
	}
	return v
}

func toCount(v any) (int, bool) {
	switch n := v.(type) {
	case float64:
		return int(n), true
	case int:
		return n, true
	}
	return 0, false
}

func (a *crAdapter) entries() ([]history.Entry, error) {
	return history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
}

func (a *crAdapter) view() (crView, error) {
	es, err := a.entries()
	if err != nil {
		return crView{}, err
	}
	return crObserve(es), nil
}

// await polls the history until ok holds.
func (a *crAdapter) await(what string, ok func(crView) bool) (crView, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		v, err := a.view()
		if err != nil {
			return v, err
		}
		if ok(v) {
			return v, nil
		}
		if time.Now().After(deadline) {
			return v, fmt.Errorf("waiting for %s: history is %+v", what, v)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *crAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	// A restart that is due (Restart is the only next step) may already
	// have happened: the actor does it in the step that made the session
	// idle.
	due := a.restart && a.changed && a.run == "up" && !v.open && !a.inflight && !a.unobserved && a.rebuilds < 1
	slack := func(got, want int) bool { return got == want || due && got == want+1 }
	if !slack(v.builds, a.builds) || v.builds != a.builds && a.store != "ok" {
		return nil, fmt.Errorf("the session built %d coordinators; the steps so far build %d", v.builds, a.builds)
	}
	if !slack(v.refusals, a.refusals) || v.refusals != a.refusals && a.store == "ok" {
		return nil, fmt.Errorf("the session refused %d builds; the steps so far refuse %d", v.refusals, a.refusals)
	}
	// After a Reply the spec's turn is open until Finish, but the actor
	// closes it as soon as the response is recorded: a history read that
	// lands after that sees the done already. Only that one done is read
	// as not yet written; any other close is still a mismatch.
	if a.finishing && !v.open && v.closes == a.closesAt+1 {
		v.open, v.dones = true, v.dones-1
	}
	return map[string]any{
		"open": v.open, "run": a.run, "down": a.down, "inflight": a.inflight,
		"unobserved": a.unobserved, "call": a.call, "restart": a.restart,
		"changed": a.changed, "rebuilds": a.rebuilds, "store": a.store,
		"ran": v.builds > 0, "crashes": v.crashes, "errs": v.errs,
		"dones": v.dones, "crashed": v.crashed, "uncounted": a.uncounted,
	}, nil
}

// ---- the model queue ----

// crSpares is how many block turns stay queued: a request the adapter
// did not expect (a superseding one) must be held too, never answered
// by llm-control's empty-queue text.
const crSpares = 3

// top moves the turns taken since the last look to takenL and queues
// more until crSpares are waiting.
func (a *crAdapter) top() {
	var left []string
	for _, n := range a.queued {
		if a.taken(n) {
			a.takenL = append(a.takenL, n)
		} else {
			left = append(left, n)
		}
	}
	a.queued = left
	for len(a.queued) < crSpares {
		n := a.name("r")
		control.Queue(a.t, a.dir, n, control.Turn{Mode: "block"})
		a.queued = append(a.queued, n)
	}
}

func (a *crAdapter) taken(name string) bool {
	_, err := os.Stat(filepath.Join(a.dir, name+".taken"))
	return err == nil
}

// current is the request in flight: the newest taken turn, unless the
// adapter already answered it. An older unanswered one was superseded.
func (a *crAdapter) current() string {
	a.top()
	if len(a.takenL) == 0 {
		return ""
	}
	if n := a.takenL[len(a.takenL)-1]; !a.released[n] {
		return n
	}
	return ""
}

// request waits for a model request in flight and returns its turn. A
// coordinator built with inputs to redeliver asks for what its store
// already had and asks again once the input lands, superseding the
// first, so it waits until no further turn is taken for a moment.
func (a *crAdapter) request() (string, error) {
	deadline := time.Now().Add(actionTimeout)
	for a.current() == "" {
		if time.Now().After(deadline) {
			return "", errors.New("no model request is in flight")
		}
		time.Sleep(10 * time.Millisecond)
	}
	for {
		n := len(a.takenL)
		time.Sleep(250 * time.Millisecond)
		if cur := a.current(); len(a.takenL) == n && cur != "" {
			return cur, nil
		}
	}
}

// answer releases the request in flight as turn says.
func (a *crAdapter) answer(turn control.Turn) error {
	cur, err := a.request()
	if err != nil {
		return err
	}
	a.released[cur] = true
	control.ReleaseWith(a.t, a.dir, cur, turn)
	return nil
}

// ---- the store ----

func (a *crAdapter) storeDir() string { return filepath.Join(a.s.Home, ".bough", "engine") }
func (a *crAdapter) logPath() string  { return filepath.Join(a.storeDir(), a.id+".session.jsonl") }
func (a *crAdapter) sysPath() string  { return filepath.Join(a.storeDir(), a.id+".system.md") }

func (a *crAdapter) breakStore() error {
	p := a.sysPath()
	if err := os.MkdirAll(a.storeDir(), 0o700); err != nil {
		return err
	}
	if _, err := os.Stat(p); err == nil {
		if err := os.Rename(p, p+".aside"); err != nil {
			return err
		}
	}
	return os.Mkdir(p, 0o700)
}

func (a *crAdapter) healStore() error {
	p := a.sysPath()
	if st, err := os.Stat(p); err == nil && st.IsDir() {
		if err := os.Remove(p); err != nil {
			return err
		}
	}
	if _, err := os.Stat(p + ".aside"); err == nil {
		return os.Rename(p+".aside", p)
	}
	return nil
}

// ---- the spec's build and rebuild, with the evidence each leaves ----

// build is the spec's build() for a step that makes the actor call
// ensureRun: it waits for the engine entry, or for the refusal's error.
func (a *crAdapter) build() error {
	if a.store == "ok" {
		a.builds++
		if _, err := a.await("a coordinator built", func(v crView) bool { return v.builds == a.builds }); err != nil {
			return err
		}
		a.run, a.down, a.changed = "up", "", false
		return nil
	}
	a.refusals++
	if _, err := a.await("the build refused", func(v crView) bool { return v.refusals == a.refusals }); err != nil {
		return err
	}
	a.down = "refused"
	return nil
}

// ---- the person ----

func (a *crAdapter) prompt(text string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.Prompt(ctx, a.id, text)
}

func (a *crAdapter) Prompt() error {
	if !a.on("Prompt", !crOpen(a)) {
		return nil
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	if err := a.prompt("prompt " + a.name("p")); err != nil {
		return err
	}
	if _, err := a.await("the prompt's input", func(v crView) bool { return v.inputs > before.inputs }); err != nil {
		return err
	}
	a.rebuilds = 0
	if a.run == "none" {
		if err := a.build(); err != nil {
			return err
		}
		if a.down == "refused" {
			_, err := a.await("the refused prompt's done", func(v crView) bool { return !v.open })
			return err
		}
	}
	a.unobserved = true
	return nil
}

func (a *crAdapter) SwitchModel() error {
	if !a.on("SwitchModel", a.run != "none" && !a.restart) {
		return nil
	}
	a.model++
	model := fmt.Sprintf("m%d", a.model%2+2)
	code, err := a.post("/model", map[string]any{"plugin": "llm-control", "model": model})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("POST /model: %d", code)
	}
	// The child writes a "model" entry once the swap is applied.
	a.models++
	if _, err := a.await("the model swap", func(v crView) bool { return v.models == a.models }); err != nil {
		return err
	}
	a.restart = true
	return nil
}

func (a *crAdapter) post(verb string, body any) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	b, _ := json.Marshal(body)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+verb, bytes.NewReader(b))
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	io.Copy(io.Discard, resp.Body)
	resp.Body.Close()
	return resp.StatusCode, nil
}

// ---- the environment ----

// McpReload toggles the wordcount row: the child hot-reloads bough.yml
// (a 300 ms debounce), the row mounts or unmounts, and the tool set
// really changes. The wait lets the signal land before the next step.
func (a *crAdapter) McpReload() error {
	if !a.on("McpReload", a.run != "none" && !(a.restart && a.changed)) {
		return nil
	}
	a.wordcount = !a.wordcount
	if err := os.WriteFile(filepath.Join(a.work, "bough.yml"), []byte(crConfig(a.wordcount)), 0o644); err != nil {
		return err
	}
	time.Sleep(450 * time.Millisecond)
	a.restart, a.changed = true, true
	return nil
}

func (a *crAdapter) StoreBreaks() error {
	if !a.on("StoreBreaks", a.store == "ok") {
		return nil
	}
	a.store = "broken"
	return a.breakStore()
}

func (a *crAdapter) StoreHeals() error {
	if !a.on("StoreHeals", a.store == "broken") {
		return nil
	}
	a.store = "ok"
	return a.healStore()
}

func (a *crAdapter) NoticeArrives() error {
	if !a.on("NoticeArrives", !crOpen(a) && a.builds > 0) {
		return nil
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	code, err := a.post("/notify", map[string]any{"text": "job " + a.name("j") + " finished"})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("POST /notify: %d", code)
	}
	if _, err := a.await("the notice's wake turn", func(v crView) bool { return v.open && v.wakes > before.wakes }); err != nil {
		return err
	}
	a.rebuilds = 0
	a.unobserved = true
	if a.run == "none" {
		return a.build()
	}
	return nil
}

// RunExitsWithError fails the store under the held request: the answer
// cannot be stored, Run returns the error, and the actor notes it and
// closes an open turn.
func (a *crAdapter) RunExitsWithError() error {
	crashes := 0
	if v, err := a.view(); err == nil {
		crashes = v.crashes
	}
	if !a.on("RunExitsWithError", a.run == "up" && crashes < 2) {
		return nil
	}
	if _, err := a.request(); err != nil {
		return err
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	if a.crashAsReply {
		if err := a.answer(control.Turn{Text: "crash"}); err != nil {
			return err
		}
		time.Sleep(300 * time.Millisecond)
	} else {
		if err := os.Chmod(a.logPath(), 0o400); err != nil {
			return err
		}
		if err := a.answer(control.Turn{Text: "lost"}); err != nil {
			return err
		}
		_, err := a.await("the Run error", func(v crView) bool { return v.crashes > before.crashes })
		os.Chmod(a.logPath(), 0o600)
		if err != nil {
			return err
		}
		if before.open {
			if _, err := a.await("the error's done", func(v crView) bool { return !v.open }); err != nil {
				return err
			}
		}
	}
	restarting := a.restart
	a.run, a.down, a.restart = "none", "error", false
	if before.open && a.call == "fg" {
		a.uncounted = true
	}
	if (restarting || a.call != "none") && a.rebuilds < 1 {
		a.rebuilds = 1
		return a.build()
	}
	return nil
}

// ---- the coordinator ----

func (a *crAdapter) Record() error {
	if !a.on("Record", a.run == "up" && a.unobserved) {
		return nil
	}
	if _, err := a.request(); err != nil {
		return err
	}
	a.unobserved, a.inflight = false, true
	return nil
}

func (a *crAdapter) Reply() error {
	if !a.on("Reply", a.run == "up" && a.inflight) {
		return nil
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	if err := a.answer(control.Turn{Text: "reply"}); err != nil {
		return err
	}
	a.inflight = false
	a.finishing, a.closesAt = !a.unobserved && a.call != "fg", before.closes
	if !before.open {
		if _, err := a.await("the reply's wake turn", func(v crView) bool { return v.wakes > before.wakes }); err != nil {
			return err
		}
		a.rebuilds = 0
	}
	return nil
}

func (a *crAdapter) CallTool() error {
	if !a.on("CallTool", a.run == "up" && a.inflight && a.call == "none") {
		return nil
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	n := a.name("g")
	a.gateF = filepath.Join(a.s.Root, n+".gate")
	a.startF = filepath.Join(a.s.Root, n+".started")
	cmd := fmt.Sprintf("touch %s; while [ ! -e %s ]; do sleep 0.05; done; echo gate open", a.startF, a.gateF)
	if err := a.answer(control.Turn{Call: &control.Call{Name: "bash", Args: map[string]any{"command": cmd}}}); err != nil {
		return err
	}
	deadline := time.Now().Add(actionTimeout)
	for {
		if _, err := os.Stat(a.startF); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return errors.New("the bash call never started")
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !before.open {
		if _, err := a.await("the call's wake turn", func(v crView) bool { return v.wakes > before.wakes }); err != nil {
			return err
		}
		a.rebuilds = 0
	}
	a.inflight, a.call = false, "fg"
	return nil
}

func (a *crAdapter) CallEnds() error {
	if !a.on("CallEnds", a.run == "up" && a.call != "none") {
		return nil
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	if err := os.WriteFile(a.gateF, nil, 0o644); err != nil {
		return err
	}
	a.gateF = ""
	if _, err := a.await("the call's recorded end", func(v crView) bool { return v.calls > before.calls }); err != nil {
		return err
	}
	// The result goes to the model: a new request, in a wake turn when
	// none is open; with one already in flight, that one.
	if _, err := a.request(); err != nil {
		return err
	}
	if !before.open {
		if _, err := a.await("the call's wake turn", func(v crView) bool { return v.open && v.wakeCalls > before.wakeCalls }); err != nil {
			return err
		}
		a.rebuilds = 0
	}
	a.call, a.inflight = "none", true
	return nil
}

// ---- the actor ----

func (a *crAdapter) Finish() error {
	if !a.on("Finish", !a.inflight && !a.unobserved && a.call != "fg") {
		return nil
	}
	_, err := a.await("the turn's done", func(v crView) bool { return !v.open })
	a.finishing = false
	return err
}

func (a *crAdapter) Adopt() error {
	if !a.on("Adopt", !a.inflight && !a.unobserved && a.call == "fg") {
		return nil
	}
	deadline := time.Now().Add(crTurnSettle + actionTimeout)
	for {
		v, err := a.view()
		if err != nil {
			return err
		}
		if !v.open {
			if v.doneRunning != 1 {
				return fmt.Errorf("the adopting done says running %d, want 1", v.doneRunning)
			}
			break
		}
		if time.Now().After(deadline) {
			return errors.New("turn_settle passed and the call was never adopted")
		}
		time.Sleep(50 * time.Millisecond)
	}
	a.call = "job"
	return nil
}

// Restart is evaluate's restart once the session is idle and the set
// has settled. For a changed set the actor cancels Run and runExit
// rebuilds in the same breath, so the evidence (an engine entry, a
// refusal, or, with the rebuild spent, nothing) is waited for here;
// RunStops, which crDrivable makes the only next step, moves the view.
func (a *crAdapter) Restart() error {
	if !a.on("Restart", a.restart && a.run == "up" && !crOpen(a) && !a.inflight && !a.unobserved) {
		return nil
	}
	if !a.changed {
		// The /model swap did mark a restart (the set moved mid-cascade);
		// evaluate drops it once toolSettle has passed with the session
		// idle. Before that a turn would defer it, and a Run error in
		// that turn would rebuild for it.
		time.Sleep(toolSettleWait)
		a.restart = false
		return nil
	}
	a.run = "stopping"
	var err error
	switch {
	case a.rebuilds >= 1:
		// No rebuild and nothing written: give the restart time to
		// happen, so a build it made against the spec is counted.
		a.stop = "none"
		time.Sleep(toolSettleWait)
	case a.store == "ok":
		a.stop = "built"
		a.builds++
		_, err = a.await("the restart's rebuild", func(v crView) bool { return v.builds == a.builds })
	default:
		a.stop = "refused"
		a.refusals++
		_, err = a.await("the restart's refused rebuild", func(v crView) bool { return v.refusals == a.refusals })
	}
	return err
}

func (a *crAdapter) RunStops() error {
	if !a.on("RunStops", a.run == "stopping") {
		return nil
	}
	a.run, a.down, a.restart = "none", "restart", false
	switch a.stop {
	case "built":
		a.rebuilds = 1
		a.run, a.down, a.changed = "up", "", false
	case "refused":
		a.rebuilds = 1
		a.down = "refused"
	}
	a.stop = ""
	return nil
}

// toolSettleWait covers the actor's 500 ms toolSettle and more.
const toolSettleWait = 1500 * time.Millisecond

// crOpen is the spec's open as the server has it.
func crOpen(a *crAdapter) bool {
	v, err := a.view()
	return err == nil && v.open
}

var crActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":            action((*crAdapter).Prompt),
	"SwitchModel":       action((*crAdapter).SwitchModel),
	"McpReload":         action((*crAdapter).McpReload),
	"StoreBreaks":       action((*crAdapter).StoreBreaks),
	"StoreHeals":        action((*crAdapter).StoreHeals),
	"NoticeArrives":     action((*crAdapter).NoticeArrives),
	"RunExitsWithError": action((*crAdapter).RunExitsWithError),
	"Record":            action((*crAdapter).Record),
	"Reply":             action((*crAdapter).Reply),
	"CallTool":          action((*crAdapter).CallTool),
	"CallEnds":          action((*crAdapter).CallEnds),
	"Finish":            action((*crAdapter).Finish),
	"Adopt":             action((*crAdapter).Adopt),
	"Restart":           action((*crAdapter).Restart),
	"RunStops":          action((*crAdapter).RunStops),
}}

// ---- which links a real server can take ----

// crDrivable says whether the walks can take action from a state with
// fields src (bare names), and why not. The actor does some steps by
// itself the moment they are enabled, so a walk that puts anything
// before them asks for an interleaving the product cannot show; others
// have no trigger from outside at all.
func crDrivable(src map[string]any, action string) (bool, string) {
	b := func(k string) bool { return src[k] == true }
	s := func(k string) string { v, _ := src[k].(string); return v }
	finishOn := b("open") && !b("inflight") && !b("unobserved") && s("call") != "fg"
	restartOn := b("restart") && b("changed") && s("run") == "up" && !b("open") && !b("inflight") && !b("unobserved")
	switch {
	case finishOn && action != "Finish":
		// evaluate closes a quiescent turn with no foreground call in
		// the same step that made it quiescent.
		return false, "finish-is-immediate"
	case restartOn && action != "Restart":
		// with the set changed and toolSettle long past, evaluate
		// restarts in the step that made the session idle.
		return false, "restart-is-immediate"
	case s("run") == "stopping" && action != "RunStops":
		// restartRun cancels Run and runExit is posted at once: the
		// window is microseconds wide.
		return false, "stop-is-immediate"
	}
	switch action {
	case "RunExitsWithError":
		// The only way to fail a running coordinator from outside is to
		// fail its store under a request the adapter holds. With a call
		// outstanding or a restart marked, runExit rebuilds at once and
		// the rebuilt coordinator hits the same read-only log.
		if !(b("inflight") || b("unobserved")) {
			return false, "crash-needs-a-request"
		}
		if s("call") != "none" || b("restart") {
			return false, "crash-rebuilds-into-the-fault"
		}
	case "CallEnds":
		// A result that lands while a request is in flight (an input
		// with a coordinator up is recorded, and asked about, at once)
		// waits for that request's answer, and the coordinator then
		// asks again for it. The spec folds it into the request in
		// flight (inflight is one bool), so after its Reply the spec
		// finishes a turn the product keeps open for the second ask.
		if b("inflight") || b("unobserved") {
			return false, "result-queues-behind-request"
		}
	case "Reply", "CallTool":
		// A coordinator built with an input to redeliver asks again for
		// the old one and supersedes that request once the input lands:
		// the stale request is never answered on its own.
		if b("unobserved") {
			return false, "stale-request-superseded"
		}
	}
	return true, ""
}

// crWalks is tracecheck.Walks over the drivable links only.
func crWalks(g *tracecheck.Graph, cover tracecheck.Cover, role string) (walks [][]tracecheck.Step, skipped map[string]int, unreached int) {
	skipped = map[string]int{}
	out := make([][]int, len(g.Nodes))
	for i, l := range g.Links {
		if l.Type != "action" {
			continue
		}
		src := map[string]any{}
		for k, v := range g.Nodes[l.Src].State {
			if f, ok := strings.CutPrefix(k, role+"."); ok {
				src[f] = v
			}
		}
		if ok, why := crDrivable(src, strings.TrimPrefix(l.Name, role+".")); !ok {
			skipped[why]++
			continue
		}
		out[l.Src] = append(out[l.Src], i)
	}
	bfs := func(from int) (map[int]int, []int) {
		parent := map[int]int{from: -1}
		order := []int{from}
		for q := 0; q < len(order); q++ {
			for _, li := range out[order[q]] {
				d := g.Links[li].Dest
				if _, ok := parent[d]; !ok {
					parent[d] = li
					order = append(order, d)
				}
			}
		}
		return parent, order
	}
	chain := func(parent map[int]int, n int) []int {
		var links []int
		for parent[n] >= 0 {
			links = append([]int{parent[n]}, links...)
			n = g.Links[parent[n]].Src
		}
		return links
	}
	reach, order := bfs(0)
	want := map[int]bool{}
	if cover == tracecheck.CoverTransitions {
		for _, n := range order {
			for _, li := range out[n] {
				want[li] = true
			}
		}
	} else {
		for _, n := range order {
			if n != 0 {
				want[n] = true
			}
		}
	}
	unreached = len(g.Nodes) - len(reach)
	const maxSteps = 50
	for len(want) > 0 {
		cur := 0
		var links []int
		for len(links) < maxSteps {
			parent, ord := bfs(cur)
			var hit []int
			for _, n := range ord {
				if cover == tracecheck.CoverStates {
					if want[n] {
						hit = chain(parent, n)
						break
					}
					continue
				}
				for _, li := range out[n] {
					if want[li] {
						hit = append(chain(parent, n), li)
						break
					}
				}
				if hit != nil {
					break
				}
			}
			if hit == nil || len(links) > 0 && len(links)+len(hit) > maxSteps {
				break
			}
			for _, li := range hit {
				delete(want, li)
				delete(want, g.Links[li].Dest)
			}
			links = append(links, hit...)
			cur = g.Links[hit[len(hit)-1]].Dest
		}
		if len(links) == 0 {
			break
		}
		steps := []tracecheck.Step{{Action: "Init", State: g.Nodes[0].State}}
		for _, li := range links {
			steps = append(steps, tracecheck.Step{Action: g.Links[li].Name, State: g.Nodes[g.Links[li].Dest].State})
		}
		walks = append(walks, steps)
	}
	return walks, skipped, unreached
}

// crWalk drives a down one trace, comparing the whole role at every node.
func crWalk(a *crAdapter, trace []tracecheck.Step) error {
	const role = "Session#0"
	var names []string
	for _, st := range trace {
		names = append(names, strings.TrimPrefix(st.Action, role+"."))
	}
	fail := func(i int, format string, args ...any) error {
		a.Cleanup()
		return fmt.Errorf("walk %v, step %d (%s): %s", names, i, names[i], fmt.Sprintf(format, args...))
	}
	for i, st := range trace {
		if i == 0 {
			if err := a.Init(); err != nil {
				return fail(i, "%v", err)
			}
		} else {
			f, ok := crActions["Session"][names[i]]
			if !ok {
				return fail(i, "no such action")
			}
			if _, err := f(a, nil); err != nil {
				return fail(i, "%v", err)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fail(i, "%v", err)
		}
		for k, v := range st.State {
			field, ok := strings.CutPrefix(k, role+".")
			if !ok {
				continue
			}
			w, _ := json.Marshal(v)
			h, _ := json.Marshal(got[field])
			if !bytes.Equal(w, h) {
				return fail(i, "%s: model %s, server %s (whole state %v)", field, w, h, got)
			}
		}
	}
	return a.Cleanup()
}

// crRun walks every drivable walk against shards serves in parallel and
// returns the first failure of each shard.
func crRun(t *testing.T, cover tracecheck.Cover, wrong bool, stopAtFirst bool) ([]*crAdapter, []error) {
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("coordinator_restart_tools_change")), "..", "testdata", "coordinator_restart_tools_change"))
	if err != nil {
		t.Fatal(err)
	}
	walks, skipped, unreached := crWalks(g, cover, "Session#0")
	steps := 0
	for _, w := range walks {
		steps += len(w) - 1
	}
	t.Logf("cover %s: %d walks, %d steps; links left out as undrivable: %v; states unreachable by drivable links: %d of %d",
		cover, len(walks), steps, skipped, unreached, len(g.Nodes))
	const shards = 4
	adapters := make([]*crAdapter, shards)
	errs := make([]error, shards)
	var wg sync.WaitGroup
	for sh := range shards {
		a := newCRAdapter(t)
		a.crashAsReply = wrong
		adapters[sh] = a
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := sh; i < len(walks); i += shards {
				if err := crWalk(a, walks[i]); err != nil {
					errs[sh] = errors.Join(errs[sh], err)
					if stopAtFirst {
						return
					}
				}
			}
		}()
	}
	wg.Wait()
	return adapters, errs
}

// The fizzbee-mbt runner picks each next action at random among all
// fifteen and stops checking at the first disabled one; almost every
// random walk of this spec ends at step one or two, and one that goes
// further asks for interleavings the actor never shows (crDrivable).
// The spec is walked by TestCoordinatorRestartToolsChangePaths.
func TestCoordinatorRestartToolsChangePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	adapters, errs := crRun(t, envCover(), false, false)
	ran := map[string]int{}
	for _, a := range adapters {
		for k, v := range a.ran {
			ran[k] += v
		}
	}
	t.Logf("actions run: %v", ran)
	if err := errors.Join(errs...); err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "coordinator_restart_tools_change"))
	if err != nil {
		t.Fatal(err)
	}
	for _, a := range adapters {
		for _, id := range a.ids {
			es := sessionHistory(t, a.s.Home, id)
			if v := g.Check(coordinatorRestartToolsChangeHistory(es)); v != nil {
				var b strings.Builder
				for _, e := range es {
					d, _ := json.Marshal(e.Data)
					if len(d) > 120 {
						d = d[:120]
					}
					fmt.Fprintf(&b, "\n  %s %s", e.Kind, d)
				}
				t.Logf("transcript that fails the trace check (%v):%s", v, b.String())
			}
			checkHistory(t, g, es, coordinatorRestartToolsChangeHistory)
		}
	}
}

// A walk whose RunExitsWithError lets the request answer normally must
// fail: otherwise the green walks prove nothing about the crash path.
func TestCoordinatorRestartToolsChangeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	_, errs := crRun(t, tracecheck.CoverStates, true, true)
	caught := 0
	for _, err := range errs {
		if err != nil {
			caught++
			t.Logf("caught: %v", err)
		}
	}
	if caught == 0 {
		t.Fatal("walks whose RunExitsWithError never fails Run passed; the walk is not checking state")
	}
}

// coordinatorRestartToolsChangeHistory reads the abstract trace off a
// transcript. Most of the spec's steps leave nothing in history (a tool
// change, the store failing and healing, a request taken, a restart
// that finds the set unchanged), so they are put back where the next
// recorded step needs them:
//
//   - an engine entry right after an input is that input's build; one
//     with no input before it is a restart for a changed tool set
//     (McpReload, Restart, RunStops), with StoreHeals first if the
//     store was last seen refusing;
//   - an "engine-unreal: " error is a refused build: of the input it
//     follows, or, with no turn open, of such a restart (StoreBreaks
//     first);
//   - an input whose build shows although the coordinator was up had
//     it stopped by a restart whose rebuild was already spent;
//   - Record goes before the first answer to an input, and a restart
//     for an unchanged set (after SwitchModel) is dropped as soon as
//     the session is idle.
func coordinatorRestartToolsChangeHistory(entries []history.Entry) []tracecheck.Step {
	const role = "Session#0."
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{role + "open": false, role + "ran": false}}}
	add := func(action string, state map[string]any) {
		st := map[string]any{}
		for k, v := range state {
			st[role+k] = v
		}
		steps = append(steps, tracecheck.Step{Action: role + action, State: st})
	}
	var (
		open, broken, restart, changed, inflight, unobserved bool
		run, call                                            = "none", "none"
		rebuilds                                             int
		closing                                              bool // the open turn's done is an error close already stepped
	)
	// outcome is what the build an input asks for did: the entry after
	// it that decides.
	outcome := func(i int) string {
		for _, e := range entries[i+1:] {
			switch e.Kind {
			case "engine":
				return "built"
			case "error":
				if t, _ := e.Data["text"].(string); strings.HasPrefix(t, "engine-unreal: ") {
					return "refused"
				}
				return "none"
			case "input", "done", "assistant", "call":
				return "none"
			}
		}
		return "none"
	}
	idle := func() bool { return !open && !inflight && !unobserved && run == "up" }
	drop := func() {
		if restart && !changed && idle() {
			add("Restart", map[string]any{"restart": false})
			restart = false
		}
	}
	// restartTo is a restart for a changed set, as its evidence shows.
	restartTo := func(result string) {
		if !(restart && changed) {
			add("McpReload", nil)
			restart, changed = true, true
		}
		switch result {
		case "built":
			if broken {
				add("StoreHeals", nil)
				broken = false
			}
		case "refused":
			if !broken {
				add("StoreBreaks", nil)
				broken = true
			}
		}
		add("Restart", map[string]any{"run": "stopping"})
		run, restart = "none", false
		if rebuilds < 1 {
			rebuilds = 1
			if result == "built" {
				run, changed = "up", false
			}
		}
		add("RunStops", map[string]any{"run": run})
	}
	// build is the build an input asks for, with the store as it shows.
	build := func(result string) {
		switch result {
		case "built":
			if broken {
				add("StoreHeals", nil)
				broken = false
			}
		case "refused":
			if !broken {
				add("StoreBreaks", nil)
				broken = true
			}
		}
	}
	record := func() {
		if unobserved && run == "up" {
			add("Record", nil)
			unobserved, inflight = false, true
		}
	}
	for i, e := range entries {
		drop()
		switch e.Kind {
		case "input":
			if st, _ := e.Data["steer"].(bool); st {
				continue
			}
			wake := e.Data["wake"] == true
			reason, _ := e.Data["reason"].(string)
			if wake && reason != "notice" {
				continue // a wake turn its call's end already opened
			}
			drop()
			res := outcome(i)
			if run == "up" && res != "none" {
				restartTo("none") // stopped, the rebuild already spent
			}
			if run == "none" {
				build(res)
			}
			rebuilds, closing = 0, false
			action := "Prompt"
			if wake {
				action = "NoticeArrives"
			}
			switch {
			case run == "up" || res == "built":
				if res == "built" {
					run, changed = "up", false
				}
				open, unobserved = true, true
				add(action, map[string]any{"open": true})
			case wake:
				// send marked it unobserved before the refused build:
				// the turn stays open.
				open, unobserved = true, true
				add(action, map[string]any{"open": true, "down": "refused"})
			default:
				open, closing = true, true
				add(action, map[string]any{"open": false, "down": "refused"})
			}
		case "assistant":
			record()
			if inflight {
				add("Reply", nil)
				inflight = false
			}
		case "call":
			if call == "none" {
				record()
				add("CallTool", nil)
				inflight, call = false, "fg"
			}
			add("CallEnds", nil)
			call, inflight = "none", true
			if !open {
				open, rebuilds = true, 0
			}
		case "error":
			text, _ := e.Data["text"].(string)
			switch {
			case strings.HasPrefix(text, "engine: "):
				add("RunExitsWithError", map[string]any{"run": "none", "open": false})
				run, restart = "none", false
				if open {
					closing = true
				}
			case strings.HasPrefix(text, "engine-unreal: ") && !open:
				restartTo("refused")
			}
		case "engine":
			if i > 0 && entries[i-1].Kind == "input" {
				continue // the build of the input before it
			}
			restartTo("built")
		case "model":
			drop()
			add("SwitchModel", nil)
			restart = true
		case "done":
			if !open {
				continue
			}
			n, _ := toCount(e.Data["running"])
			switch {
			case closing:
				// the refused build's or the Run error's close
			case n > 0:
				if call == "none" {
					record()
					add("CallTool", nil)
					inflight, call = false, "fg"
				}
				add("Adopt", map[string]any{"open": false, "call": "job"})
				call = "job"
			default:
				record()
				add("Finish", map[string]any{"open": false})
			}
			open, closing = false, false
			drop()
		}
	}
	return steps
}

// The projection must be able to fail: a transcript whose turn never
// wrote its done is not a path in the model, while the same transcript
// with the done is.
func TestCoordinatorRestartToolsChangeHistoryCatchesMissingDone(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("coordinator_restart_tools_change")), "..", "testdata", "coordinator_restart_tools_change"))
	if err != nil {
		t.Fatal(err)
	}
	turn := func(done bool) []history.Entry {
		es := []history.Entry{
			{Kind: "input", Data: map[string]any{"text": "one"}},
			{Kind: "engine", Data: map[string]any{}},
			{Kind: "assistant", Data: map[string]any{"text": "reply"}},
		}
		if done {
			es = append(es, history.Entry{Kind: "done", Data: map[string]any{}})
		}
		return es
	}
	var good, bad []history.Entry
	good = append(append(good, turn(true)...), history.Entry{Kind: "input", Data: map[string]any{"text": "two"}})
	bad = append(append(bad, turn(false)...), history.Entry{Kind: "input", Data: map[string]any{"text": "two"}})
	if v := g.Check(coordinatorRestartToolsChangeHistory(good)); v != nil {
		t.Fatalf("a whole turn and a second prompt: %v", v)
	}
	v := g.Check(coordinatorRestartToolsChangeHistory(bad))
	if v == nil {
		t.Fatal("a turn with no done passed the trace check")
	}
	t.Logf("caught: %v", v)
}

func init() {
	historyProjections["coordinator_restart_tools_change"] = coordinatorRestartToolsChangeHistory
}
