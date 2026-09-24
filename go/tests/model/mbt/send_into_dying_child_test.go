//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/send_into_dying_child.fizz against a real serve: a prompt, an
// answer or a /model written to a session's child while it is exiting
// (the SIGINT tail) or has exited with its lease still held (dead).
//
// Both windows last milliseconds in a real run, so the serve runs with
// BOUGH_TEST_HOLD_DIR (internal/testhold), which opens three holds:
//
//   - every stdin line a child reads parks at <pid>.line.<n> until the
//     adapter writes <pid>.line.<n>.go. That is how a line stays
//     "queued" (written, not yet read by the loop), how the adapter picks
//     TailTake or TailModelRead over Exit (let it go in the tail, or
//     never), and how an answer sent after Stop's SIGINT is never read by
//     the turn (AnswerWhileSignalled: the adapter lets the cancel win).
//     Each line waits on its own goroutine, but stdin is FIFO, so release
//     refuses to let a line go ahead of an earlier one (the spec's
//     modelFirst says which of a prompt and a /model is ahead).
//   - a child SIGINTed out of a turn parks at <pid>.tail after its
//     "cancelled" and before Unmount (cmd/bough/main.go): the tail.
//     Removing the file is Exit.
//   - serve parks a child's exit goroutine at <pid>.drop once both pipes
//     are drained, before cmd.Wait, the "exit" event and drop: dead.
//     Removing the file is Drop.
//
// The adapter creates each hold before the step that reaches it and
// waits for the process's <name>.reached, so child is read off where
// the real process is parked, never off the adapter's intent. status,
// ask and meta.Model are read off the server; line and model off
// history and the parked lines; send, stopping and sigint are the
// page's, checked against each POST's status code.
//
// Stop on a running turn is the page's POST, but its SIGINT is "on its
// way" until ChildSignal, where the adapter makes the POST (as in
// stop_interrupt_test.go), so AnswerWhileSignalled and Crash can land in
// between.
type sidcAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	hold string // BOUGH_TEST_HOLD_DIR
	gate gate

	id  string
	ids []string
	pid int // the session's current child, 0 when it has none
	n   int // texts and turn names, unique in this serve

	held string // the llm-control turn the running turn holds
	next string // the turn queued for the request after an answer

	send     string // the page: "", "accepted", "failed"
	prompt   string // the page's current prompt text, "" when none
	sigint   bool
	stopping string
	lostAsk  int    // 1 + the history index of an ask answered with a 200 its turn never read
	lostText string // that answer, parked for good
	// modelFirst is the spec's: the /model line went into the pipe ahead
	// of the prompt line. release checks the pipe's order against it.
	modelFirst bool

	// exitReadsLine is TestSendIntoDyingChildCatchesWrongAdapter's bug:
	// Exit lets the tail's parked lines go before the child unmounts.
	exitReadsLine bool
}

func newSidcAdapter(t *testing.T) *sidcAdapter {
	hold := t.TempDir()
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"BOUGH_TEST_HOLD_DIR=" + hold}})
	return &sidcAdapter{t: t, s: s, dir: control.Dir(s.Home), hold: hold}
}

func (a *sidcAdapter) name(prefix string) string {
	a.n++
	return fmt.Sprintf("%s%05d", prefix, a.n)
}

// Init is a fresh session with a live, idle child.
func (a *sidcAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, a.name("w")), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.held, a.next, a.send, a.prompt, a.sigint, a.stopping = "", "", "", "", false, ""
	a.lostAsk, a.lostText, a.modelFirst = 0, "", false
	a.gate.reset()
	pid, err := a.childPID()
	if err != nil {
		return err
	}
	a.pid = pid
	return nil
}

// childPID finds the session's child among serve's: Cleanup archives
// every earlier walk's, so there is one.
func (a *sidcAdapter) childPID() (int, error) {
	deadline := time.Now().Add(10 * time.Second)
	for {
		out, _ := exec.Command("pgrep", "-P", strconv.Itoa(a.s.PID()), "-f", "--", "--headless").Output()
		f := strings.Fields(string(out))
		if len(f) == 1 {
			return strconv.Atoi(f[0])
		}
		if time.Now().After(deadline) {
			return 0, fmt.Errorf("serve %d has %d headless children, want 1", a.s.PID(), len(f))
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Cleanup lets every hold go, archives (kills) the child and empties
// the llm queue, so the next walk starts clean.
func (a *sidcAdapter) Cleanup() error {
	a.clearHolds()
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	ents, _ := os.ReadDir(a.hold)
	for _, e := range ents {
		os.Remove(filepath.Join(a.hold, e.Name()))
	}
	a.pid = 0
	a.dropQueue()
	return nil
}

// clearHolds removes the tail and drop holds, so nothing stays parked.
func (a *sidcAdapter) clearHolds() {
	ents, _ := os.ReadDir(a.hold)
	for _, e := range ents {
		if n := e.Name(); strings.HasSuffix(n, ".tail") || strings.HasSuffix(n, ".drop") {
			os.Remove(filepath.Join(a.hold, n))
		}
	}
}

// dropQueue takes back turns nobody will take and releases the taken
// ones, so a later request never takes a stale turn.
func (a *sidcAdapter) dropQueue() {
	ents, _ := os.ReadDir(a.dir)
	for _, e := range ents {
		n := e.Name()
		switch {
		case strings.HasSuffix(n, ".json"):
			os.Remove(filepath.Join(a.dir, n))
		case strings.HasSuffix(n, ".taken"):
			if _, err := os.Stat(filepath.Join(a.dir, strings.TrimSuffix(n, ".taken")+".release")); err != nil {
				control.Release(a.t, a.dir, strings.TrimSuffix(n, ".taken"))
			}
		}
	}
	a.held, a.next = "", ""
}

func (a *sidcAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// parked is a stdin line a child read into its hold.
type parked struct {
	pid  int
	path string
	gone bool // let go
}

// line finds the parked line with text, preferring the current child's.
func (a *sidcAdapter) line(text string) (parked, bool) {
	matches, _ := filepath.Glob(filepath.Join(a.hold, "*.line.*"))
	var found parked
	ok := false
	for _, m := range matches {
		if strings.HasSuffix(m, ".go") {
			continue
		}
		b, err := os.ReadFile(m)
		if err != nil || string(b) != text {
			continue
		}
		pid, _ := strconv.Atoi(strings.SplitN(filepath.Base(m), ".", 2)[0])
		_, err = os.Stat(m + ".go")
		p := parked{pid: pid, path: m, gone: err == nil}
		if !ok || pid == a.pid {
			found, ok = p, true
		}
	}
	return found, ok
}

// waitLine waits for a child to read text into its hold.
func (a *sidcAdapter) waitLine(text string) (parked, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		if p, ok := a.line(text); ok {
			return p, nil
		}
		if time.Now().After(deadline) {
			return parked{}, fmt.Errorf("no child read %q within %s", text, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// release lets a parked line go. stdin is FIFO, so a line still parked
// ahead of it in the same child means the walk asked for an order no
// real pipe has: an error, not a reorder. The answer the SIGINT beat is
// the exception: the child reads it after the cancel and drops it,
// which leaving it parked stands for.
func (a *sidcAdapter) release(text string) error {
	p, ok := a.line(text)
	if !ok || p.gone {
		return fmt.Errorf("no parked line %q to let go", text)
	}
	n := lineNo(p.path)
	matches, _ := filepath.Glob(filepath.Join(a.hold, fmt.Sprintf("%d.line.*", p.pid)))
	for _, m := range matches {
		if strings.HasSuffix(m, ".go") || exists(m+".go") || lineNo(m) >= n {
			continue
		}
		if b, _ := os.ReadFile(m); a.lostText == "" || string(b) != a.lostText {
			return fmt.Errorf("letting %q go ahead of %q, which is earlier in the pipe", text, b)
		}
	}
	return os.WriteFile(p.path+".go", nil, 0o644)
}

func lineNo(path string) int {
	n, _ := strconv.Atoi(path[strings.LastIndex(path, ".")+1:])
	return n
}

func (a *sidcAdapter) holdFile(name string) string {
	return filepath.Join(a.hold, fmt.Sprintf("%d.%s", a.pid, name))
}

func exists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

// waitFile waits for p to exist.
func waitFile(p string, d time.Duration) error {
	deadline := time.Now().Add(d)
	for !exists(p) {
		if time.Now().After(deadline) {
			return fmt.Errorf("%s did not appear within %s", filepath.Base(p), d)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// sidcView is one read of the server.
type sidcView struct {
	row     serve.Row
	entries []history.Entry
	child   string
	armed   bool
}

func (a *sidcAdapter) view() (sidcView, error) {
	var v sidcView
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
	switch {
	case !row.Live:
		v.child = "none"
	case exists(a.holdFile("drop.reached")):
		v.child = "dead"
	case exists(a.holdFile("tail.reached")):
		v.child = "tail"
	default:
		v.child = "alive"
	}
	// serve's arm, probed without touching it (as in ask_answer_test.go):
	// an answer naming another question is refused as expired when one
	// is armed, and as no pending ask when none is.
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

// where names where a written line is: read by the loop is "", and an
// unread one is queued, tail or lost by the state of the child it went to.
func (a *sidcAdapter) where(v sidcView, text string) string {
	p, ok := a.line(text)
	if !ok {
		return ""
	}
	if p.pid != a.pid {
		return "lost"
	}
	switch v.child {
	case "alive":
		return "queued"
	case "tail":
		return "tail"
	}
	return "lost"
}

func landed(entries []history.Entry, kind, text string) bool {
	for _, e := range entries {
		if e.Kind == kind && str(e.Data["text"]) == text {
			return true
		}
	}
	return false
}

func recordedModel(entries []history.Entry) bool {
	for _, e := range entries {
		if e.Kind == "model" {
			return true
		}
	}
	return false
}

const sidcModel = "/model b"

func (a *sidcAdapter) state(v sidcView) map[string]any {
	status := string(v.row.Status)
	if v.row.Status == serve.StatusNeedsYou {
		status = "running"
	}
	send, line := a.send, ""
	if a.prompt != "" && send == "accepted" {
		if landed(v.entries, "input", a.prompt) {
			send = ""
		} else {
			line = a.where(v, a.prompt)
		}
	}
	model := "a"
	if v.row.Model == "b" {
		switch {
		case recordedModel(v.entries):
			model = "b"
		default:
			model = a.where(v, sidcModel)
			if model == "" {
				model = "lost" // let go, but no "model" entry: never ran
			}
		}
	}
	lost := false
	if a.lostAsk > 0 && a.lostAsk <= len(v.entries) {
		lost = true
		id := str(v.entries[a.lostAsk-1].Data["id"])
		for _, e := range v.entries[a.lostAsk:] {
			if e.Kind == "ask/answer" && str(e.Data["id"]) == id {
				lost = false
			}
		}
	}
	pending := model == "queued" || model == "tail"
	return map[string]any{
		"child": v.child, "status": status, "sigint": a.sigint, "ask": v.armed,
		"lostAnswer": lost, "send": send, "line": line, "model": model, "stopping": a.stopping,
		"modelFirst": a.modelFirst && pending,
	}
}

func (a *sidcAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return a.state(v), nil
}

// post is a POST to /api/sessions/<id>/<verb>: its status and error text.
func (a *sidcAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
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

// cur is the state the adapter acts on, read off the server.
func (a *sidcAdapter) cur() (sidcView, map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return v, nil, err
	}
	return v, a.state(v), nil
}

// deliver POSTs a line through ensure and checks what the spec's
// deliver says it answers: a 500 (EPIPE) into a dead child, else a 200
// and the line parked in the child it reached. A respawn (child none)
// also closes a dangling turn, which the -r child does as it boots.
func (a *sidcAdapter) deliver(v sidcView, verb, text string, body any) (ok bool, err error) {
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, verb, body)
	if err != nil {
		return false, err
	}
	if v.child == "dead" {
		if code != http.StatusInternalServerError {
			return false, fmt.Errorf("%s into an exited child: %d %s, want 500", verb, code, msg)
		}
		return false, nil
	}
	if code != http.StatusOK {
		return false, fmt.Errorf("%s: %d %s, want 200", verb, code, msg)
	}
	p, err := a.waitLine(text)
	if err != nil {
		return false, err
	}
	if v.child == "none" {
		a.pid = p.pid
		if v.row.Status == serve.StatusInterrupted {
			if _, err := waitRow(a.s, a.id, "the respawn to close the dangling turn", func(r serve.Row) bool {
				return r.Status != serve.StatusInterrupted
			}); err != nil {
				return false, err
			}
		}
	} else if p.pid != a.pid {
		return false, fmt.Errorf("%s reached child %d, the session's is %d", verb, p.pid, a.pid)
	}
	return true, nil
}

// sendPrompt is Prompt, Retry and Resend: the page's POST /prompt.
func (a *sidcAdapter) sendPrompt(v sidcView, text string) error {
	model := a.state(v)["model"]
	ok, err := a.deliver(v, "prompt", text, map[string]string{"text": text})
	if err != nil {
		return err
	}
	a.prompt = text
	if ok {
		a.send = "accepted"
		if model == "queued" || model == "tail" {
			a.modelFirst = true
		}
	} else {
		a.send = "failed"
	}
	return nil
}

func (a *sidcAdapter) Prompt() error {
	v, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["send"] == "" && st["status"] != "running" && a.stopping == "") {
		return nil
	}
	return a.sendPrompt(v, a.name("prompt-"))
}

func (a *sidcAdapter) Retry() error {
	v, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["send"] == "failed") {
		return nil
	}
	return a.sendPrompt(v, a.prompt)
}

func (a *sidcAdapter) Edit() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["send"] == "failed") {
		return nil
	}
	a.send, a.prompt = "", ""
	return nil
}

func (a *sidcAdapter) Resend() error {
	v, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["send"] == "accepted" && st["line"] == "lost" && st["child"] == "none") {
		return nil
	}
	return a.sendPrompt(v, a.name("resend-"))
}

func (a *sidcAdapter) SetModel() error {
	v, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["model"] == "a" && st["ask"] == false) {
		return nil
	}
	ok, err := a.deliver(v, "model", sidcModel, map[string]string{"model": "b"})
	a.modelFirst = ok && st["line"] != "queued" && st["line"] != "tail"
	return err
}

// Answer is POST /answer; the child reads the line, the ask returns and
// the engine's next request takes the turn queued at Ask.
func (a *sidcAdapter) Answer() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["ask"] == true && st["child"] == "alive" && !a.sigint) {
		return nil
	}
	text := a.name("answer-")
	if err := a.answer(text); err != nil {
		return err
	}
	if err := a.release(text); err != nil {
		return err
	}
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.held, a.next = a.next, ""
	_, err = waitRow(a.s, a.id, "the answered turn to run on", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// answer POSTs text as the answer and waits for the child to read it
// into its hold.
func (a *sidcAdapter) answer(text string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": text})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("answer: %d %s, want 200", code, msg)
	}
	_, err = a.waitLine(text)
	return err
}

// AnswerWhileSignalled: the same POST after Stop, and the line is left
// parked: the cancel ChildSignal makes wins, and the line dies with the
// process.
func (a *sidcAdapter) AnswerWhileSignalled() error {
	v, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["ask"] == true && st["child"] == "alive" && a.sigint) {
		return nil
	}
	ask := 0
	for i, e := range v.entries {
		if e.Kind == "ask" {
			ask = i + 1
		}
	}
	text := a.name("answer-")
	if err := a.answer(text); err != nil {
		return err
	}
	a.lostAsk, a.lostText = ask, text
	return nil
}

func (a *sidcAdapter) Stop() error {
	v, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass((st["send"] == "accepted" || st["status"] == "running") && a.stopping != "stopping" &&
		!(st["child"] == "alive" && st["line"] == "queued")) {
		return nil
	}
	a.stopping = "stopping"
	if v.child == "alive" {
		a.sigint = true // POSTed at ChildSignal
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	err = a.s.Interrupt(ctx, a.id)
	var e *servetest.APIError
	if v.child == "none" {
		if !errors.As(err, &e) || e.Status != http.StatusNotFound {
			return fmt.Errorf("stop with no child: got %v, want 404", err)
		}
		a.stopping = "failed"
		return nil
	}
	if err != nil {
		return fmt.Errorf("stop a %s child: %w, want 200", v.child, err)
	}
	return nil
}

// Take lets the prompt's parked line go: the loop records its input and
// the turn runs on a held model request.
func (a *sidcAdapter) Take() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["line"] == "queued" && st["child"] == "alive" && !(st["model"] == "queued" && st["modelFirst"] == true)) {
		return nil
	}
	return a.take("the prompt to become a running turn")
}

func (a *sidcAdapter) take(what string) error {
	a.held = a.name("t")
	control.Queue(a.t, a.dir, a.held, control.Turn{Mode: "block", Text: "finished " + a.held})
	if err := a.release(a.prompt); err != nil {
		return err
	}
	if err := waitTaken(a.dir, a.held); err != nil {
		return err
	}
	_, err := a.wait(what, func(r serve.Row, es []history.Entry) bool {
		return landed(es, "input", a.prompt) && r.Status == serve.StatusRunning
	})
	a.lostAsk = 0
	return err
}

// wait polls the row and history until ok holds.
func (a *sidcAdapter) wait(what string, ok func(serve.Row, []history.Entry) bool) (sidcView, error) {
	deadline := time.Now().Add(actionTimeout)
	for {
		v, err := a.view()
		if err == nil && ok(v.row, v.entries) {
			return v, nil
		}
		if time.Now().After(deadline) {
			var kinds []string
			for _, e := range v.entries {
				kinds = append(kinds, e.Kind)
			}
			return v, fmt.Errorf("waiting for %s: row %s live=%v child %s, history %v (err %v)", what, v.row.Status, v.row.Live, v.child, kinds, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (a *sidcAdapter) ModelRead() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["model"] == "queued" && st["child"] == "alive" && !(st["line"] == "queued" && st["modelFirst"] == false) &&
		!(st["lostAnswer"] == true && a.sigint)) {
		return nil
	}
	return a.readModel()
}

func (a *sidcAdapter) readModel() error {
	if err := a.release(sidcModel); err != nil {
		return err
	}
	_, err := a.wait("the /model line to be recorded", func(_ serve.Row, es []history.Entry) bool { return recordedModel(es) })
	return err
}

// Ask releases the held request as an ask call; the request after the
// answer takes the turn queued here.
func (a *sidcAdapter) Ask() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["status"] == "running" && st["child"] == "alive" && !a.sigint && st["ask"] == false && st["model"] != "queued") {
		return nil
	}
	a.next = a.name("t")
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: "ask", Args: map[string]any{"question": "Which colour?", "options": []string{"red", "blue"}}})
	a.held = ""
	_, err = waitRow(a.s, a.id, "needs-you", func(r serve.Row) bool { return r.Status == serve.StatusNeedsYou })
	return err
}

func (a *sidcAdapter) Finish() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["status"] == "running" && st["child"] == "alive" && !a.sigint && st["ask"] == false) {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err = waitRow(a.s, a.id, "the turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone })
	return err
}

// ChildSignal is Stop's POST reaching the child. A running turn is
// cancelled and the child parks in its tail, with the drop hold ready
// for Exit; an idle child exits at once and parks in serve's drop hold.
func (a *sidcAdapter) ChildSignal() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.sigint && v.child == "alive") {
		return nil
	}
	running := v.row.Status == serve.StatusRunning || v.row.Status == serve.StatusNeedsYou
	holds, reach := []string{"drop"}, "drop.reached"
	if running {
		holds, reach = []string{"tail", "drop"}, "tail.reached"
	}
	for _, h := range holds {
		if err := os.WriteFile(a.holdFile(h), nil, 0o644); err != nil {
			return err
		}
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return fmt.Errorf("stop: %w", err)
	}
	if err := waitFile(a.holdFile(reach), stopBound); err != nil {
		return fmt.Errorf("the SIGINTed child never reached %s: %w", reach, err)
	}
	a.sigint = false
	if !running {
		return nil
	}
	a.dropQueue()
	_, err = a.wait("the stop to cancel the running turn", func(r serve.Row, es []history.Entry) bool {
		return r.Status == serve.StatusStopped
	})
	return err
}

// TailTake lets the tail's prompt line go before Unmount: the loop
// records it and starts a turn the unmount will kill.
func (a *sidcAdapter) TailTake() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["line"] == "tail" && st["child"] == "tail" && !(st["model"] == "tail" && st["modelFirst"] == true)) {
		return nil
	}
	return a.take("the tail to take the prompt")
}

func (a *sidcAdapter) TailModelRead() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["model"] == "tail" && st["child"] == "tail" && !(st["line"] == "tail" && st["modelFirst"] == false)) {
		return nil
	}
	return a.readModel()
}

// Exit lets the tail unmount and exit; what it did not read stays
// parked and dies with it.
func (a *sidcAdapter) Exit() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.child == "tail") {
		return nil
	}
	if a.exitReadsLine {
		for _, text := range []string{a.prompt, sidcModel} {
			if p, ok := a.line(text); ok && !p.gone && p.pid == a.pid {
				os.WriteFile(p.path+".go", nil, 0o644)
			}
		}
		time.Sleep(200 * time.Millisecond)
	}
	os.Remove(a.holdFile("tail"))
	if err := waitFile(a.holdFile("drop.reached"), actionTimeout); err != nil {
		return fmt.Errorf("the tail never exited: %w", err)
	}
	a.dropQueue()
	return nil
}

// Crash SIGKILLs the live child; serve parks its exit before drop.
func (a *sidcAdapter) Crash() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.child == "alive") {
		return nil
	}
	if err := os.WriteFile(a.holdFile("drop"), nil, 0o644); err != nil {
		return err
	}
	if err := syscall.Kill(a.pid, syscall.SIGKILL); err != nil {
		return fmt.Errorf("kill child %d: %w", a.pid, err)
	}
	if err := waitFile(a.holdFile("drop.reached"), actionTimeout); err != nil {
		return err
	}
	a.sigint = false
	a.dropQueue()
	return nil
}

// Drop lets serve's exit goroutine run: the exit event, then drop.
func (a *sidcAdapter) Drop() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.child == "dead") {
		return nil
	}
	os.Remove(a.holdFile("drop"))
	if _, err := waitRow(a.s, a.id, "the lease to drop", func(r serve.Row) bool { return !r.Live }); err != nil {
		return err
	}
	os.Remove(a.holdFile("drop.reached"))
	os.Remove(a.holdFile("tail.reached"))
	return nil
}

// Settle is the page once nothing is live.
func (a *sidcAdapter) Settle() error {
	_, st, err := a.cur()
	if err != nil {
		return err
	}
	if a.gate.pass(st["send"] != "accepted" && st["status"] != "running" && a.stopping != "") {
		a.stopping = ""
	}
	return nil
}

var sidcActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":               action((*sidcAdapter).Prompt),
	"Retry":                action((*sidcAdapter).Retry),
	"Edit":                 action((*sidcAdapter).Edit),
	"Resend":               action((*sidcAdapter).Resend),
	"SetModel":             action((*sidcAdapter).SetModel),
	"Answer":               action((*sidcAdapter).Answer),
	"AnswerWhileSignalled": action((*sidcAdapter).AnswerWhileSignalled),
	"Stop":                 action((*sidcAdapter).Stop),
	"Take":                 action((*sidcAdapter).Take),
	"ModelRead":            action((*sidcAdapter).ModelRead),
	"Ask":                  action((*sidcAdapter).Ask),
	"Finish":               action((*sidcAdapter).Finish),
	"ChildSignal":          action((*sidcAdapter).ChildSignal),
	"TailTake":             action((*sidcAdapter).TailTake),
	"TailModelRead":        action((*sidcAdapter).TailModelRead),
	"Exit":                 action((*sidcAdapter).Exit),
	"Crash":                action((*sidcAdapter).Crash),
	"Drop":                 action((*sidcAdapter).Drop),
	"Settle":               action((*sidcAdapter).Settle),
}}

func sidcOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 10, "max-parallel-runs": 0}
}

// compare checks the adapter's state against a path's qualified state.
func (a *sidcAdapter) compare(want map[string]any) error {
	got, err := a.GetState()
	if err != nil {
		return err
	}
	var diff []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Session#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[f]) != fmt.Sprint(v) {
			diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
		}
	}
	if len(diff) > 0 {
		sort.Strings(diff)
		return fmt.Errorf("state: %s", strings.Join(diff, "; "))
	}
	return nil
}

// transcript is the session's history, one short word per entry, for a
// failure message.
func (a *sidcAdapter) transcript() string {
	var kinds []string
	if es, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl")); err == nil {
		for _, e := range es {
			k := e.Kind
			if t := str(e.Data["text"]); t != "" && len(t) < 20 {
				k += "(" + t + ")"
			}
			kinds = append(kinds, k)
		}
	}
	return fmt.Sprint(kinds)
}

// sidcShards is how many serves walk the paths side by side.
const sidcShards = 4

// walkSidcPaths drives the walks over the checked-in graph through
// sidcShards adapters, each on its own serve, and compares the state
// after every step. stopAtFirst ends the run at the first mismatch.
func walkSidcPaths(t *testing.T, cover tracecheck.Cover, stopAtFirst bool, setup func(*sidcAdapter)) (mismatches []string, adapters []*sidcAdapter) {
	t.Helper()
	b, err := pathsJSONCover("send_into_dying_child", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []stopInterruptPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	var mu sync.Mutex
	failed := false
	var wg sync.WaitGroup
	for sh := 0; sh < sidcShards; sh++ {
		a := newSidcAdapter(t)
		if setup != nil {
			setup(a)
		}
		adapters = append(adapters, a)
		wg.Add(1)
		go func(sh int, a *sidcAdapter) {
			defer wg.Done()
			for pi := sh; pi < len(doc.Paths); pi += sidcShards {
				mu.Lock()
				stop := stopAtFirst && failed
				mu.Unlock()
				if stop {
					return
				}
				p := doc.Paths[pi]
				var names []string
				for si, step := range p.Trace {
					name := strings.TrimPrefix(step.Action, "Session#0.")
					names = append(names, name)
					var err error
					if si == 0 {
						err = a.Init()
					} else if f, ok := sidcActions["Session"][name]; !ok {
						err = fmt.Errorf("no adapter action %q", name)
					} else {
						_, err = f(a, nil)
					}
					if err == nil {
						err = a.compare(step.State)
					}
					if err != nil {
						mu.Lock()
						mismatches = append(mismatches, fmt.Sprintf("path %d step %d (%s): %v\nhistory: %s", pi, si, strings.Join(names, " "), err, a.transcript()))
						failed = true
						mu.Unlock()
						break
					}
				}
				if err := a.Cleanup(); err != nil {
					mu.Lock()
					mismatches = append(mismatches, fmt.Sprintf("path %d: cleanup: %v", pi, err))
					failed = true
					mu.Unlock()
					return
				}
			}
		}(sh, a)
	}
	wg.Wait()
	t.Logf("send_into_dying_child: %d walks (%s) over %d serves", len(doc.Paths), cover, sidcShards)
	return mismatches, adapters
}

// sendIntoDyingChildHistory reads the abstract trace off a transcript.
// History has no process boundaries, no page and nothing a line that was
// never read left behind, so the projection infers the fewest steps that
// explain what was recorded:
//
//   - a "cancelled" closing a running turn is a Stop and its ChildSignal:
//     the child is now in its tail. A "cancelled" {interrupted} is the
//     respawn closing a turn a dead child left open: a Crash and a Drop
//     before it.
//   - in the tail, an input the same process ran (no "engine" entry
//     after it: a new process writes one at its first turn) is a
//     TailTake, and the "cancelled" closing it is the Exit. A "model"
//     entry there is a TailModelRead.
//   - anything a new process records first gets the tail's Exit and the
//     lease's Drop, and a Settle before the page sends again.
//
// A crash of an idle child, a line lost with its process, a refused send
// and an answer the SIGINT beat record nothing, and read as a path that
// never did them, which the graph also has.
func sendIntoDyingChildHistory(entries []history.Entry) []tracecheck.Step {
	status := func(s string) map[string]any { return map[string]any{"Session#0.status": s} }
	steps := []tracecheck.Step{{Action: "Init", State: status("idle")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	child, st, stopping, tailTurn := "alive", "idle", false, false
	settle := func() {
		if stopping {
			add("Settle", nil)
			stopping = false
		}
	}
	// gone makes the lease free: the tail exits, the dead child drops.
	gone := func() {
		if child == "tail" {
			add("Exit", nil)
			child = "dead"
		}
		if child == "dead" {
			add("Drop", nil)
			child = "none"
			if st == "running" {
				st = "interrupted"
			}
		}
	}
	// ensure is the spec's: a respawn closes a dangling turn as stopped.
	ensure := func() {
		if child == "none" {
			child = "alive"
			if st == "interrupted" {
				st = "stopped"
			}
		}
	}
	newProcessAt := func(i int) bool {
		for _, e := range entries[i+1:] {
			switch e.Kind {
			case "meta", "origin":
				continue
			case "engine":
				return true
			}
			return false
		}
		return false
	}
	for i, e := range entries {
		switch e.Kind {
		case "input":
			if e.Data["steer"] != nil {
				continue
			}
			settle()
			if child == "tail" && !newProcessAt(i) {
				add("Prompt", nil)
				st, tailTurn = "running", true
				add("TailTake", status(st))
				continue
			}
			if newProcessAt(i) {
				gone()
			}
			ensure()
			add("Prompt", status(st))
			st = "running"
			add("Take", status(st))
		case "ask":
			add("Ask", nil)
		case "ask/answer":
			add("Answer", nil)
		case "done":
			if st == "running" && !tailTurn {
				st = "done"
				add("Finish", status(st))
			}
		case "cancelled":
			switch {
			case e.Data["interrupted"] == true:
				if child == "alive" {
					add("Crash", nil)
					child = "dead"
				}
				gone()
			case tailTurn:
				st, tailTurn, child = "stopped", false, "dead"
				add("Exit", status(st))
			case st == "running" && child == "alive":
				add("Stop", nil)
				st, stopping, child = "stopped", true, "tail"
				add("ChildSignal", status(st))
			}
		case "model":
			if child == "tail" {
				add("SetModel", nil)
				add("TailModelRead", map[string]any{"Session#0.model": "b"})
				continue
			}
			if child == "dead" {
				gone()
			}
			ensure()
			add("SetModel", nil)
			add("ModelRead", map[string]any{"Session#0.model": "b"})
		}
	}
	return steps
}

func init() { historyProjections["send_into_dying_child"] = sendIntoDyingChildHistory }

func TestSendIntoDyingChildPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	mismatches, adapters := walkSidcPaths(t, envCover(), false, nil)
	for _, m := range mismatches {
		t.Error(m)
	}
	g, err := tracecheck.Load(fizzCheck(t, "send_into_dying_child"))
	if err != nil {
		t.Fatal(err)
	}
	for _, a := range adapters {
		for _, id := range a.ids {
			checkHistory(t, g, sessionHistory(t, a.s.Home, id), sendIntoDyingChildHistory)
		}
	}
}

// The projection on transcripts the walks write, and on one no walk can:
// a turn that was never closed before the next prompt.
func TestSendIntoDyingChildHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, "send_into_dying_child"))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string, data map[string]any) history.Entry { return history.Entry{Kind: kind, Data: data} }
	in := func(text string) history.Entry { return e("input", map[string]any{"text": text}) }
	meta, engine, done := e("meta", nil), e("engine", nil), e("done", nil)
	cancelled := e("cancelled", nil)
	for name, es := range map[string][]history.Entry{
		// Stop, a prompt the tail took, its exit, a respawned turn.
		"tail take": {meta, in("p1"), engine, cancelled, done, in("p2"), cancelled, done, in("p3"), engine, done},
		// A crash mid-turn, a /model respawn closing it, an answered ask.
		"crash then model": {meta, in("p1"), engine, e("cancelled", map[string]any{"interrupted": true}),
			e("command", map[string]any{"text": sidcModel}), e("model", nil), e("system", nil),
			in("p2"), engine, e("ask", nil), e("ask/answer", nil), done},
		// A /model the tail read, then a prompt for a new process.
		"tail model": {meta, in("p1"), engine, cancelled, done, e("model", nil), in("p2"), engine},
	} {
		if v := g.Check(sendIntoDyingChildHistory(es)); v != nil {
			t.Errorf("%s: a transcript the walks write is refused: %v", name, v)
		}
	}
	unclosed := []history.Entry{meta, in("p1"), engine, e("assistant", nil), in("p2")}
	if v := g.Check(sendIntoDyingChildHistory(unclosed)); v == nil || !strings.Contains(v.Reason, "not enabled") {
		t.Fatalf("a prompt into a running turn: Check = %v, want Prompt not enabled", v)
	}
}

// The runner's random walks over the same adapter: part of the
// exhaustive run only (runMBT skips otherwise).
func TestSendIntoDyingChild(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSidcAdapter(t)
	if err := runMBT(t, "send_into_dying_child", a, sidcActions, sidcOptions()); err != nil {
		t.Errorf("model-based run: %v", err)
	}
}

// An Exit that lets the tail read its parked lines before it unmounts
// must fail: the line the spec loses lands (or the /model is recorded).
// Only the Exit transitions out of a tail holding a line show it, so the
// walks cover every transition, and stop at the first mismatch.
func TestSendIntoDyingChildCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	mismatches, _ := walkSidcPaths(t, tracecheck.CoverTransitions, true, func(a *sidcAdapter) { a.exitReadsLine = true })
	if len(mismatches) == 0 {
		t.Fatal("walks whose Exit lets the tail read its lines passed; the adapter is not checking state")
	}
	t.Logf("caught: %s", mismatches[0])
}
