//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"sort"
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

// specs/ask_beside_parallel_calls.fizz against a real serve: the model's
// one reply is a native ask plus a run_js (tools: both) or a bash call,
// both started at once by the engine, and the sibling's end, a stderr
// line, an error note and a provider error land while the ask is open.
//
// How each spec event is made real:
//
//   - The reply is a held llm-control request released with both calls
//     (Turn.Calls on a release). The sibling polls for a file of the
//     walk's own and ends ok or failed on what it finds there, so
//     SiblingEndOk and SiblingEndFail happen at the step the path says.
//   - The gap between the ask's call starting and its question being put
//     in (asker "pending") is held open by a "hold" file in
//     BOUGH_TEST_ASK_EXPIRE_DIR (plugins/ask/native.go); AskOpens removes
//     it. Timeout is a file named for the ask's id in the same dir.
//   - Every model request the engine makes is held (one block turn is
//     always queued), and only Finish lets the held ones answer, so the
//     turn never ends behind the walk's back. inflight is whether one is
//     held. ProviderFails fails the one in flight.
//   - StderrLine is a real line on the child's stderr, printed by the
//     llm-control row on cue, which serve relays as an "error" event.
//   - HookErrorNote is a recorded "error" entry mid-turn: the request in
//     flight is released as a provider refusal, which the engine notes as
//     an error.
//
// What is read off the server: status (the row), ask (the row's ask),
// armed (serve's arm, probed with an /answer naming no real question),
// asker, sibling, perr, noted, steeredPast and msgAnswered (history),
// inflight (a request taken from llm-control and not answered), queued
// and unsent (a steer, or a call's end, recorded after the request in
// flight was taken: the engine holds it for that request's answer; sent
// at once it would have made a request of its own). The
// child's hlAsk has no read-out: hl is reported as armed, and a hlAsk
// left behind shows on the next SendMessage as msgAnswered or
// steeredPast, both read off history. tool and sent are the adapter's
// own; lost is computed at Timeout from the state read just before it.

// abpcConfig is llm-control with run_js on the engine.
const abpcConfig = controlConfig + "- id: loop\n  plugin: engine-unreal\n  config:\n    tools: both\n"

const abpcPerr = "abpc provider boom"

// abpcGrace outlasts the harness's one-second grace for a reply's calls.
const abpcGrace = 1200 * time.Millisecond

type abpcAdapter struct {
	t      *testing.T
	s      *servetest.Server
	dir    string // llm-control's queue
	expire string // BOUGH_TEST_ASK_EXPIRE_DIR
	gate   gate

	id, cwd string
	ids     []string
	walk    int
	turn    int
	n       int      // message and answer markers
	next    string   // the block turn queued for the next request
	held    []string // requests taken and not yet answered, oldest first
	heldAt  int      // history length when the newest held one was taken

	tool    string // "" | "js" | "bash"
	askCall string // the reply's call ids
	sibCall string
	sibFile string // the sibling ends when this holds "ok" or "fail"
	sibSaid string // what the adapter wrote there
	sent    bool
	lost    bool
	did     map[string]int

	// siblingOkAsFail is the wrong-adapter test's bug: SiblingEndOk
	// ends the sibling as a failure.
	siblingOkAsFail bool
}

func newAbpcAdapter(t *testing.T) *abpcAdapter {
	expire := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: abpcConfig,
		Env:    []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire},
	})
	return &abpcAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, did: map[string]int{}}
}

func (a *abpcAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("t%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

// absorb moves a taken queued turn onto held and queues the next one.
func (a *abpcAdapter) absorb() bool {
	if _, err := os.Stat(filepath.Join(a.dir, a.next+".taken")); err != nil {
		return false
	}
	a.held = append(a.held, a.next)
	es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	a.heldAt = len(es)
	a.queueNext()
	return true
}

// release answers the newest held request as turn says ("" answers with
// its own text).
func (a *abpcAdapter) release(turn *control.Turn) error {
	if len(a.held) == 0 {
		return errors.New("no model request is held")
	}
	name := a.held[len(a.held)-1]
	a.held = a.held[:len(a.held)-1]
	if turn == nil {
		control.Release(a.t, a.dir, name)
	} else {
		control.ReleaseWith(a.t, a.dir, name, *turn)
	}
	return nil
}

func (a *abpcAdapter) Init() error {
	a.walk++
	a.queueNext()
	if err := os.WriteFile(filepath.Join(a.expire, "hold"), nil, 0o644); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", a.walk))
	row, err := a.s.CreateSession(ctx, a.cwd, "start "+a.next)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, nil
	a.ids = append(a.ids, row.ID)
	a.tool, a.sent, a.lost, a.sibSaid = "", false, false, ""
	a.askCall, a.sibCall = fmt.Sprintf("ask_w%d", a.walk), fmt.Sprintf("sib_w%d", a.walk)
	a.sibFile = filepath.Join(a.cwd, ".sibling")
	a.gate.reset()
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.absorb()
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// Cleanup ends the walk's child (Archive kills it), lets the sibling
// go, and takes back the queued turn and any held one, so the next
// walk's requests are its own.
func (a *abpcAdapter) Cleanup() error {
	if a.sibSaid == "" && a.sibFile != "" {
		os.WriteFile(a.sibFile, []byte("ok"), 0o644)
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if a.id != "" {
		if _, err := a.s.Archive(ctx, a.id); err != nil {
			return err
		}
		if _, err := waitRow(a.s, a.id, "the child to be gone", func(r serve.Row) bool { return !r.Live }); err != nil {
			return err
		}
	}
	for _, h := range a.held {
		control.Release(a.t, a.dir, h)
	}
	a.held = nil
	if a.next != "" {
		os.Remove(filepath.Join(a.dir, a.next+".json"))
		a.next = ""
	}
	ents, _ := os.ReadDir(a.expire)
	for _, e := range ents {
		os.Remove(filepath.Join(a.expire, e.Name()))
	}
	return nil
}

func (a *abpcAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// abpcView is one read of the server.
type abpcView struct {
	row     serve.Row
	entries []history.Entry
	asker   string
	askID   string
	sibling string
	armed   bool
	perr    bool
	noted   bool
	steered bool
	msgAns  bool
	queued  bool
	unsent  bool
}

func (a *abpcAdapter) view() (abpcView, error) {
	var v abpcView
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return v, err
	}
	v.row = row
	v.entries, err = history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return v, err
	}
	v.asker, v.sibling = "none", "none"
	if a.tool != "" {
		v.asker, v.sibling = "pending", "running"
	}
	first := true
	for i, e := range v.entries {
		if len(a.held) > 0 && i >= a.heldAt && a.callEnd(e) {
			v.unsent = true
		}
		switch e.Kind {
		case "input":
			if first {
				first = false
				break
			}
			// StatusOf reads errors from the last input on, a steer's too.
			v.perr, v.noted = false, false
			if e.Data["steer"] == true && len(a.held) > 0 && i >= a.heldAt {
				v.queued = true
			}
			if v.asker == "open" || v.asker == "job" {
				v.steered = true
			}
		case "ask":
			v.asker, v.askID = "open", str(e.Data["id"])
		case "ask/answer":
			if str(e.Data["id"]) == v.askID && v.askID != "" {
				v.asker = "answered"
				if strings.HasPrefix(str(e.Data["text"]), "msg-") {
					v.msgAns = true
				}
			}
		case "job":
			if str(e.Data["call"]) == a.askCall && e.Data["event"] == "started" && v.asker == "open" {
				v.asker = "job"
			}
		case "error":
			if strings.Contains(str(e.Data["text"]), abpcPerr) {
				v.perr = true
			}
			if e.Data["refusal"] != nil {
				v.noted = true
			}
		case "call":
			if e.Data["phase"] == "start" {
				break
			}
			switch str(e.Data["id"]) {
			case a.askCall:
				if v.asker == "answered" {
					break
				}
				msg := str(e.Data["error"])
				switch {
				case e.Data["canceled"] == true || strings.Contains(msg, "cancelled"):
					v.asker = "cancelled"
				case strings.Contains(msg, "no answer after"):
					v.asker = "timedout"
				default:
					v.asker = "ended:" + msg
				}
			case a.sibCall:
				v.sibling = a.siblingEnd(e)
			}
		case "result":
			if a.tool == "js" {
				v.sibling = a.siblingEnd(e)
			}
		}
	}
	// serve's arm, probed without touching it: an answer naming no real
	// question is refused as expired while an ask is armed, and as no
	// pending ask when none is.
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": "", "ask": "probe-not-an-ask"})
	switch {
	case err != nil:
		return v, err
	case code == http.StatusConflict && strings.Contains(msg, "expired"):
		v.armed = true
	case code == http.StatusConflict && strings.Contains(msg, "no pending ask"):
	default:
		return v, fmt.Errorf("arm probe: %d %s", code, msg)
	}
	return v, nil
}

// callEnd says whether e is the recorded end of one of the reply's
// calls: the ask's answer or its call's end, or the sibling's.
func (a *abpcAdapter) callEnd(e history.Entry) bool {
	switch e.Kind {
	case "ask/answer":
		return true
	case "result":
		return a.tool == "js"
	case "call":
		id := str(e.Data["id"])
		return e.Data["phase"] != "start" && (id == a.askCall || id == a.sibCall)
	}
	return false
}

// siblingEnd reads the sibling's recorded end: failed or ok on what the
// adapter told it, cancelled when it ended before being told anything.
func (a *abpcAdapter) siblingEnd(e history.Entry) string {
	switch {
	case a.sibSaid == "":
		return "cancelled"
	case e.Data["error"] != nil && str(e.Data["error"]) != "" || e.Data["exit"] != nil && fmt.Sprint(e.Data["exit"]) != "0":
		return "failed"
	}
	return "ok"
}

func (a *abpcAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"status":      string(v.row.Status),
		"asker":       v.asker,
		"armed":       v.armed,
		"hl":          v.armed,
		"ask":         v.row.Ask != nil,
		"sibling":     v.sibling,
		"tool":        a.tool,
		"perr":        v.perr,
		"noted":       v.noted,
		"inflight":    len(a.held) > 0,
		"queued":      v.queued,
		"unsent":      v.unsent,
		"sent":        a.sent,
		"steeredPast": v.steered,
		"msgAnswered": v.msgAns,
		"lost":        a.lost,
	}, nil
}

func (a *abpcAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
	b, _ := json.Marshal(body)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+a.id+"/"+verb, strings.NewReader(string(b)))
	if err != nil {
		return 0, "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	var e struct {
		Error string `json:"error"`
	}
	json.NewDecoder(resp.Body).Decode(&e)
	return resp.StatusCode, e.Error, nil
}

// settle waits until the transcript, the row and the llm queue have
// been still for a moment, absorbing any request the engine made: the
// engine answers a step with several asynchronous events (a call's end,
// a new request, a steer landing), and the state is compared after all
// of them.
func (a *abpcAdapter) settle() error {
	const quiet = 250 * time.Millisecond
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
		sig := fmt.Sprintf("%s %v %d %d", row.Status, row.Live, len(lines), len(a.held))
		if sig != last {
			last, lastAt = sig, time.Now()
		} else if time.Since(lastAt) >= quiet {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("the session did not settle in %s", actionTimeout)
}

// waitEntry waits for an entry past the first n that ok accepts.
func (a *abpcAdapter) waitEntry(n int, what string, ok func(history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
		for i := n; i < len(es); i++ {
			if ok(es[i]) {
				return nil
			}
		}
		a.absorb()
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("waiting for %s: not in history after %s", what, actionTimeout)
}

func (a *abpcAdapter) reply(tool string) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && a.tool == "" && len(a.held) > 0) {
		return nil
	}
	sib := control.Call{ID: a.sibCall}
	wait := fmt.Sprintf("while [ ! -s %q ]; do sleep 0.02; done", a.sibFile)
	if tool == "js" {
		sib.Name = "run_js"
		sib.Args = map[string]any{"code": fmt.Sprintf("const r = tools.bash(%q + '; cat ' + %q);\nif (r.includes('fail')) throw new Error('sibling failed');\n'sibling ok';", wait, a.sibFile)}
	} else {
		sib.Name = "bash"
		sib.Args = map[string]any{"command": fmt.Sprintf("%s; grep -q fail %q && exit 3; echo sibling ok", wait, a.sibFile)}
	}
	ask := control.Call{ID: a.askCall, Name: "ask", Args: map[string]any{"question": "Which colour?", "options": []string{"red", "blue"}}}
	if err := a.release(&control.Turn{Calls: []control.Call{ask, sib}}); err != nil {
		return err
	}
	a.tool = tool
	// The harness holds a reply's finished calls back for a second
	// (coordinator toolCallRunGracePeriod) waiting for the others; the
	// spec's reply includes that second, so a sibling's end in a later
	// step asks the model again as it does in real time.
	time.Sleep(abpcGrace)
	return a.settle()
}

func (a *abpcAdapter) ReplyWithAskAndRunJS() error { return a.reply("js") }
func (a *abpcAdapter) ReplyWithAskAndBash() error  { return a.reply("bash") }

func (a *abpcAdapter) AskOpens() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.asker == "pending") {
		return nil
	}
	if err := os.Remove(filepath.Join(a.expire, "hold")); err != nil {
		return err
	}
	if err := a.waitEntry(len(v.entries), "the ask", func(e history.Entry) bool { return e.Kind == "ask" }); err != nil {
		return err
	}
	return a.settle()
}

func (a *abpcAdapter) siblingEnd2(ok bool) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.sibling == "running") {
		return nil
	}
	a.sibSaid = "ok"
	if !ok {
		a.sibSaid = "fail"
	}
	if err := os.WriteFile(a.sibFile, []byte(a.sibSaid), 0o644); err != nil {
		return err
	}
	if err := a.waitEntry(len(v.entries), "the sibling's end", func(e history.Entry) bool {
		return (a.tool == "js" && e.Kind == "result") || (e.Kind == "call" && str(e.Data["id"]) == a.sibCall && e.Data["phase"] != "start")
	}); err != nil {
		return err
	}
	return a.settle()
}

func (a *abpcAdapter) SiblingEndOk() error   { return a.siblingEnd2(!a.siblingOkAsFail) }
func (a *abpcAdapter) SiblingEndFail() error { return a.siblingEnd2(false) }

func (a *abpcAdapter) ProviderFails() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.asker == "open" && len(a.held) > 0 && !v.perr) {
		return nil
	}
	if err := a.release(&control.Turn{Mode: "error", Error: abpcPerr}); err != nil {
		return fmt.Errorf("provider fails: %w", err)
	}
	if err := a.waitEntry(len(v.entries), "the provider error", func(e history.Entry) bool {
		return e.Kind == "error" && strings.Contains(str(e.Data["text"]), abpcPerr)
	}); err != nil {
		return err
	}
	return a.settle()
}

// StderrLine makes the child print a line on its stderr, through the
// llm-control row (control.Stderr), which serve relays as a live
// "error". A config reload prints one too, but it also reconciles the
// tree, and the engine then asked the model again with the sibling's
// result: not the event this step is about.
func (a *abpcAdapter) StderrLine() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.asker == "open") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	st, err := a.s.Events(ctx, a.id)
	if err != nil {
		return err
	}
	defer st.Close()
	a.n++
	line := fmt.Sprintf("abpc stderr line %d", a.n)
	control.Stderr(a.t, a.dir, fmt.Sprintf("w%d-%d", a.walk, a.n), line)
	if _, err := st.WaitFor(ctx, func(ev serve.Event) bool { return ev.Kind == "error" && strings.Contains(ev.Text, line) }); err != nil {
		return fmt.Errorf("the stderr line: %w", err)
	}
	return a.settle()
}

// HookErrorNote records an error note mid-turn: the request in flight
// is refused.
func (a *abpcAdapter) HookErrorNote() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.asker == "open" && len(a.held) > 0) {
		return nil
	}
	if err := a.release(&control.Turn{Mode: "refuse", Error: "abpc refusal note"}); err != nil {
		return err
	}
	if err := a.waitEntry(len(v.entries), "the refusal note", func(e history.Entry) bool {
		return e.Kind == "error" && strings.Contains(str(e.Data["text"]), "abpc refusal note")
	}); err != nil {
		return err
	}
	return a.settle()
}

func (a *abpcAdapter) Answer() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusNeedsYou) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	a.n++
	id := ""
	if v.row.Ask != nil {
		id = v.row.Ask.ID
	}
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": fmt.Sprintf("ans-%d", a.n), "ask": id})
	if err != nil {
		return err
	}
	switch {
	case code == http.StatusConflict:
		// Refused: the page reports it and nothing changed.
		return a.settle()
	case code != http.StatusOK:
		return fmt.Errorf("answer: %d %s", code, msg)
	}
	if err := a.waitEntry(len(v.entries), "the answer", func(e history.Entry) bool { return e.Kind == "ask/answer" }); err != nil {
		return err
	}
	return a.settle()
}

func (a *abpcAdapter) SendMessage() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.sent && v.row.Status != serve.StatusNeedsYou) {
		return nil
	}
	a.sent = true
	ctx, cancel := actionCtx()
	defer cancel()
	a.n++
	text := fmt.Sprintf("msg-%d", a.n)
	code, msg, err := a.post(ctx, "prompt", map[string]string{"text": text})
	if err != nil {
		return err
	}
	switch {
	case code == http.StatusConflict:
		return a.settle()
	case code != http.StatusOK:
		return fmt.Errorf("prompt: %d %s", code, msg)
	}
	if err := a.waitEntry(len(v.entries), "the message", func(e history.Entry) bool {
		return (e.Kind == "input" || e.Kind == "ask/answer") && strings.Contains(str(e.Data["text"]), text)
	}); err != nil {
		return err
	}
	if v.row.Status != serve.StatusRunning {
		// A new turn: its request is held.
		if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
			return err
		}
	}
	return a.settle()
}

func (a *abpcAdapter) Timeout() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.asker == "open" || v.asker == "job") {
		return nil
	}
	if !(v.armed && v.row.Ask != nil && v.asker == "open") {
		a.lost = true
	}
	if err := os.WriteFile(filepath.Join(a.expire, v.askID), nil, 0o644); err != nil {
		return err
	}
	if err := a.waitEntry(len(v.entries), "the ask's timed-out end", func(e history.Entry) bool {
		return e.Kind == "call" && str(e.Data["id"]) == a.askCall && e.Data["phase"] != "start"
	}); err != nil {
		return err
	}
	return a.settle()
}

func (a *abpcAdapter) Stop() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning || v.row.Status == serve.StatusNeedsYou) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return fmt.Errorf("stop: %w", err)
	}
	// A turn with an error in it reads error, not stopped (StatusOf).
	if _, err := waitRow(a.s, a.id, "the stop", func(r serve.Row) bool {
		return (r.Status == serve.StatusStopped || r.Status == serve.StatusError) && !r.Live
	}); err != nil {
		return err
	}
	// The child is gone and its requests with it.
	for _, h := range a.held {
		control.Release(a.t, a.dir, h)
	}
	a.held = nil
	return a.settle()
}

// Finish lets every held request answer until the turn ends.
func (a *abpcAdapter) Finish() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && v.asker != "none" && v.asker != "pending" && v.asker != "open" && v.sibling != "running") {
		return nil
	}
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		a.absorb()
		ctx, cancel := actionCtx()
		row, _, err := a.s.GetSession(ctx, a.id)
		cancel()
		if err != nil {
			return err
		}
		if row.Status != serve.StatusRunning {
			return a.settle()
		}
		if len(a.held) > 0 {
			if err := a.release(nil); err != nil {
				return err
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	return errors.New("finish: the turn did not end")
}

func abpcCounted(name string, f func(*abpcAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*abpcAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var abpcActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"ReplyWithAskAndRunJS": abpcCounted("ReplyWithAskAndRunJS", (*abpcAdapter).ReplyWithAskAndRunJS),
	"ReplyWithAskAndBash":  abpcCounted("ReplyWithAskAndBash", (*abpcAdapter).ReplyWithAskAndBash),
	"AskOpens":             abpcCounted("AskOpens", (*abpcAdapter).AskOpens),
	"SiblingEndOk":         abpcCounted("SiblingEndOk", (*abpcAdapter).SiblingEndOk),
	"SiblingEndFail":       abpcCounted("SiblingEndFail", (*abpcAdapter).SiblingEndFail),
	"ProviderFails":        abpcCounted("ProviderFails", (*abpcAdapter).ProviderFails),
	"StderrLine":           abpcCounted("StderrLine", (*abpcAdapter).StderrLine),
	"HookErrorNote":        abpcCounted("HookErrorNote", (*abpcAdapter).HookErrorNote),
	"Answer":               abpcCounted("Answer", (*abpcAdapter).Answer),
	"SendMessage":          abpcCounted("SendMessage", (*abpcAdapter).SendMessage),
	"Timeout":              abpcCounted("Timeout", (*abpcAdapter).Timeout),
	"Stop":                 abpcCounted("Stop", (*abpcAdapter).Stop),
	"Finish":               abpcCounted("Finish", (*abpcAdapter).Finish),
	// fizz links a state with nothing enabled to itself as "end"; the
	// exhaustive walks take it last, and nothing happens.
	"end": abpcEnd,
}, "": {"end": abpcEnd}}

func abpcEnd(any, []fmbt.Arg) (any, error) { return nil, nil }

func abpcOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// abpcHistory reads the abstract trace off a transcript. The reply is
// the run_js's "code" entry, or for a bash sibling (recorded only at its
// end) the first entry of the reply's calls; the ask entry is AskOpens;
// the sibling's end is its result or its bash call (a native call has a
// string id, a call inside a run_js a number); the ask's call ending
// with no answer is Timeout; an answer is Answer; a later input is
// SendMessage; a provider error or a refusal note mid-turn is
// ProviderFails or HookErrorNote; cancelled is Stop and the done after
// it bookkeeping. A stderr line leaves nothing and changes nothing. The
// check is on status and ask.
func abpcHistory(entries []history.Entry) []tracecheck.Step {
	st := func(status string, ask bool) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.ask": ask}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("running", false)}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	first, replied, open, turnErr, cancelled := true, false, false, false, false
	reply := func(tool string) {
		if !replied {
			replied = true
			add("ReplyWithAskAnd"+tool, st("running", false))
		}
	}
	running := func() map[string]any {
		if open {
			return st("needs-you", true)
		}
		return st("running", false)
	}
	for _, e := range entries {
		id, native := e.Data["id"].(string)
		errText := str(e.Data["error"])
		ended := e.Kind == "call" && e.Data["phase"] != "start"
		cancel := e.Data["canceled"] == true || strings.Contains(errText, "cancelled") || strings.Contains(errText, "Cancelled")
		switch {
		case e.Kind == "input" && first:
			first = false
		case e.Kind == "input":
			add("SendMessage", st("running", false))
			turnErr = false
			cancelled = false
		case e.Kind == "code":
			reply("RunJS")
		case e.Kind == "ask":
			reply("Bash")
			open = true
			add("AskOpens", st("needs-you", true))
		case e.Kind == "ask/answer" && open:
			open = false
			add("Answer", st("running", false))
		case ended && native && (str(e.Data["tool"]) == "ask" || str(e.Data["tool"]) == "secret"):
			reply("Bash")
			if open && !cancel {
				open = false
				add("Timeout", st("running", false))
			}
		case ended && native && str(e.Data["tool"]) == "bash" && id != "", e.Kind == "result":
			if e.Kind != "result" {
				reply("Bash")
			}
			if cancel {
				break
			}
			if errText != "" || (e.Data["exit"] != nil && fmt.Sprint(e.Data["exit"]) != "0") {
				add("SiblingEndFail", running())
			} else {
				add("SiblingEndOk", running())
			}
		case e.Kind == "error" && e.Data["refusal"] != nil:
			turnErr = true
			add("HookErrorNote", running())
		case e.Kind == "error" && open:
			turnErr = true
			add("ProviderFails", running())
		case e.Kind == "cancelled":
			open, cancelled = false, true
			add("Stop", st(closedAs("stopped", turnErr), false))
		case e.Kind == "done" && !cancelled:
			add("Finish", st(closedAs("done", turnErr), false))
		}
	}
	return steps
}

// closedAs is StatusOf on a closed turn: an error in it outranks how it
// closed.
func closedAs(status string, errored bool) string {
	if errored {
		return "error"
	}
	return status
}

func init() { historyProjections["ask_beside_parallel_calls"] = abpcHistory }

// abpcPaths is the walks over the checked-in graph.
func abpcPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("ask_beside_parallel_calls", cover)
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

// walkAbpcPath drives one path and compares the whole state after every
// step; the first difference ends the path.
func walkAbpcPath(a *abpcAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int, want map[string]any) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		var diff []string
		for k, v := range want {
			field, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(got[field]) != fmt.Sprint(v) {
				diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", field, v, got[field]))
			}
		}
		if len(diff) > 0 {
			sort.Strings(diff)
			return fmt.Errorf("step %d (%s): %s", i, strings.TrimPrefix(path[i].Action, "Session#0."), strings.Join(diff, "; "))
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
		name := strings.TrimPrefix(path[i].Action, "Session#0.")
		fn, ok := abpcActions["Session"][name]
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

func abpcDump(a *abpcAdapter) string {
	es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	var b strings.Builder
	for _, e := range es {
		d, _ := json.Marshal(e.Data)
		if len(d) > 300 {
			d = d[:300]
		}
		fmt.Fprintf(&b, "    %s %s\n", e.Kind, d)
	}
	return b.String()
}

func pathNames(p []tracecheck.Step) []string {
	var acts []string
	for _, st := range p[1:] {
		acts = append(acts, strings.TrimPrefix(st.Action, "Session#0."))
	}
	return acts
}

// TestAskBesideParallelCallsPaths walks every generated path against
// real serves (four, sharing the paths), then replays each walk's
// transcript on the graph.
func TestAskBesideParallelCallsPaths(t *testing.T) {
	t.Parallel()
	paths := abpcPaths(t, envCover())
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newAbpcAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkAbpcPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v\n%s", i, pathNames(paths[i]), err, abpcDump(a))
				}
			}
			t.Logf("steps taken: %v", a.did)
			g := loadAbpcGraph(t)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), abpcHistory)
			}
		})
	}
}

// The path walk proves nothing unless a wrongly wired adapter fails it:
// a SiblingEndOk that fails the sibling must show on the first path
// that takes it.
func TestAskBesideParallelCallsPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newAbpcAdapter(t)
	a.siblingOkAsFail = true
	for _, p := range abpcPaths(t, tracecheck.CoverTransitions) {
		if !strings.Contains(strings.Join(pathNames(p), " "), "SiblingEndOk") {
			continue
		}
		err := walkAbpcPath(a, p)
		if err == nil {
			t.Fatalf("path %v passed with SiblingEndOk failing the sibling; the walk is not checking state", pathNames(p))
		}
		if !strings.Contains(err.Error(), "SiblingEndOk") || !strings.Contains(err.Error(), "sibling: spec ok, got failed") {
			t.Fatalf("path %v failed, but not on the wrong sibling: %v", pathNames(p), err)
		}
		return
	}
	t.Fatal("no path takes SiblingEndOk")
}

// TestAskBesideParallelCalls is the runner's random walks, part of the
// exhaustive run.
func TestAskBesideParallelCalls(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAbpcAdapter(t)
	if err := runMBT(t, "ask_beside_parallel_calls", a, abpcActions, abpcOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g := loadAbpcGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), abpcHistory)
	}
}

func loadAbpcGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("ask_beside_parallel_calls")), "..", "testdata", "ask_beside_parallel_calls"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}
