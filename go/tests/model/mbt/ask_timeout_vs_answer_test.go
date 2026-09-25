//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/internal/stepgate"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/ask_timeout_vs_answer.fizz: one tools.ask or tools.secret whose
// timeout, a Stop or stdin's end races a late answer from the web,
// against a real serve. The races live in gaps a few instructions wide,
// so each step is held open through internal/stepgate
// (BOUGH_TEST_STEP_GATE, a folder in each walk's own cwd, shared by the
// child and serve's side of it):
//
//	recv     the Asker's select may take the answer channel
//	giveup   the timeout or done branch, chosen, deletes pending
//	store    a secret's value is stored in the (file) keychain
//	ret      the native call's end is recorded and printed (ChildEnd)
//	scanend  serve's pump reads that end and disarms (ScanEnd)
//	in.<n>   the child's stdin reader takes line n (ReadLine)
//
// and three files act: the ask id in BOUGH_TEST_ASK_EXPIRE_DIR fires
// the timeout, "cancel" cancels the turn as a SIGINT does (without the
// exit serve's Stop also costs, which stop_interrupt.fizz covers), and
// "eof" makes serve drop the child and close its stdin. The state no
// API shows (the Asker's pending entry and channel, the child's hlAsk
// and hlErrored) is read off the notes those hooks write; the rest off
// the API, history and the keychain file.

const atvaGate = ".stepgate" // relative: each child gates in its own cwd

type atvaAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	expire   string
	keychain string
	gate     gate

	id, cwd    string
	turn, n    int
	held, next string

	// The page's own state: the question it shows (and its id), and the
	// POST /answer it has sent. read is how many stdin lines the child
	// has been let to take.
	shown          bool
	shownID        string
	post, postText string
	read           int

	ids []string
	did map[string]int

	// storeFailsStores is the deliberate bug the wrong-adapter test
	// injects: StoreFails lets the store run on a working keychain.
	storeFailsStores bool
}

func newAtvaAdapter(t *testing.T) *atvaAdapter {
	expire, keychain := t.TempDir(), t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files:  map[string]string{".bough/projects/" + askProject + "/project.yml": "name: " + askProject + "\n"},
		Env: []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire, "BOUGH_TEST_KEYCHAIN_DIR=" + keychain,
			stepgate.Env + "=" + atvaGate},
	})
	return &atvaAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, keychain: keychain, did: map[string]int{}}
}

func (a *atvaAdapter) file(name string) string { return filepath.Join(a.cwd, atvaGate, name) }

func (a *atvaAdapter) has(name string) bool {
	_, err := os.Stat(a.file(name))
	return err == nil
}

func (a *atvaAdapter) note(key string) string {
	b, _ := os.ReadFile(a.file(key + ".note"))
	return string(b)
}

// let passes the hold name and waits for until.
func (a *atvaAdapter) let(name, what string, until func() bool) error {
	if err := atvaWait(name+" to be held", func() bool { return a.has(name + ".held") }); err != nil {
		return err
	}
	if err := os.WriteFile(a.file(name+".go"), nil, 0o644); err != nil {
		return err
	}
	return atvaWait(what, until)
}

func atvaWait(what string, ok func() bool) error {
	deadline := time.Now().Add(actionTimeout)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: not after %s", what, actionTimeout)
		}
		time.Sleep(5 * time.Millisecond)
	}
	return nil
}

func (a *atvaAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("v%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

// Init starts each walk on a fresh session whose first turn is running,
// its model request held, with one more turn queued for whatever
// request the engine makes after the ask.
func (a *atvaAdapter) Init() error {
	os.Remove(filepath.Join(a.keychain, keychainFile()))
	a.queueNext()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("v%d", len(a.ids)))
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.cwd, "start "+a.next)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	a.ids = append(a.ids, row.ID)
	a.shown, a.shownID, a.post, a.postText, a.read = false, "", "", "", 0
	a.gate.reset()
	if err := a.let("in.0", "the first prompt to be read", func() bool { return a.has("in.0.done") }); err != nil {
		return err
	}
	a.read = 1
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.held = a.next
	a.queueNext()
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// Cleanup opens every hold, kills the child and takes back the turn it
// had queued.
func (a *atvaAdapter) Cleanup() error {
	if a.cwd != "" {
		os.WriteFile(a.file("open"), nil, 0o644)
	}
	if a.id != "" {
		k := &askAnswerAdapter{s: a.s, id: a.id, cwd: a.cwd}
		if err := k.kill(); err != nil {
			return err
		}
	}
	a.held = ""
	if a.next != "" {
		os.Remove(filepath.Join(a.dir, a.next+".json"))
		a.next = ""
	}
	ents, _ := os.ReadDir(a.expire)
	for _, e := range ents {
		os.Remove(filepath.Join(a.expire, e.Name()))
	}
	// StoreFails leaves a directory where the keychain file goes.
	os.Remove(filepath.Join(a.keychain, keychainFile()))
	return nil
}

// atvaView is the spec's state as one step reads it, plus what the
// actions need.
type atvaView struct {
	row     serve.Row
	entries []history.Entry
	askID   string
	st      map[string]any
}

func (a *atvaAdapter) view() (atvaView, error) {
	var v atvaView
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
	kind, hist, steer := "", false, false
	for i, e := range v.entries {
		switch e.Kind {
		case "ask":
			kind, v.askID = "plain", str(e.Data["id"])
			if s, _ := e.Data["secret"].(bool); s {
				kind = "secret"
			}
		case "ask/answer":
			hist = true
		}
		// A late answer the child read with no question open went to
		// the loop as a message: it carries the answer's marker.
		if i > 0 && (e.Kind == "input" || e.Kind == "steer") && a.postText != "" {
			if b, _ := json.Marshal(e.Data); strings.Contains(string(b), a.postText) {
				steer = true
			}
		}
	}
	// serve's arm, probed without touching it: an answer naming no real
	// question is refused as expired while one is armed.
	code, msg, err := (&askAnswerAdapter{s: a.s, id: a.id}).post(ctx, "answer", map[string]string{"text": "", "ask": "probe-not-an-ask"})
	arm := false
	switch {
	case err != nil:
		return v, err
	case code == http.StatusConflict && strings.Contains(msg, "expired"):
		arm = true
	case code == http.StatusConflict && strings.Contains(msg, "no pending ask"):
	default:
		return v, fmt.Errorf("arm probe: %d %s", code, msg)
	}
	end := ""
	switch {
	case a.has("scanend.done"):
		end = "seen"
	case a.has("scanend.held"):
		end = "out"
	}
	var lost []string
	if a.note("unread") == "true" {
		lost = append(lost, "unread")
	}
	if a.note("refused") == "true" {
		lost = append(lost, "stderr")
	}
	if steer {
		lost = append(lost, "steer")
	}
	stored := false
	if b, err := os.ReadFile(filepath.Join(a.keychain, keychainFile())); err == nil && strings.HasPrefix(string(b), "S3CR3T-") {
		stored = true
	}
	in := fmt.Sprintf("in.%d", a.read)
	v.st = map[string]any{
		"kind":    kind,
		"call":    a.note("call"),
		"pending": a.note("pending") == "true",
		"chan":    a.note("chan"),
		"hist":    hist,
		"hlAsk":   a.note("hlAsk") == "true",
		"end":     end,
		"arm":     arm,
		"shown":   a.shown,
		"post":    a.post,
		"line":    a.has(in+".held") && !a.has(in+".done"),
		"eof":     !row.Live,
		"errored": a.note("errored") == "true",
		"lost":    strings.Join(lost, "+"),
		"stored":  stored,
	}
	return v, nil
}

func (a *atvaAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	return v.st, err
}

// Each action asks the gate with the spec's require first.

func (a *atvaAdapter) ask(tool string, args map[string]any) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["kind"] == "") {
		return nil
	}
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: tool, Args: args})
	a.held = ""
	row, err := waitRow(a.s, a.id, "the question", func(r serve.Row) bool { return r.Ask != nil })
	if err != nil {
		return err
	}
	// The page shows it on the poll that saw it.
	a.shown, a.shownID = true, row.Ask.ID
	return atvaWait("the child and serve to hold the question", func() bool {
		v, err := a.view()
		return err == nil && v.st["call"] == "open" && v.st["hlAsk"] == true && v.st["arm"] == true
	})
}

func (a *atvaAdapter) AskPlain() error {
	return a.ask("ask", map[string]any{"question": "Which colour?"})
}

func (a *atvaAdapter) AskSecret() error {
	return a.ask("secret", map[string]any{"name": askSecret, "question": "the API token", "project": askProject})
}

// fire makes the select choose the timeout (or the run's done): it then
// holds before the delete.
func (a *atvaAdapter) fire(trigger func(v atvaView) error) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["call"] == "open") {
		return nil
	}
	if err := trigger(v); err != nil {
		return err
	}
	return atvaWait("the select to choose", func() bool { return a.has("giveup.held") && a.note("call") == "fired" })
}

func (a *atvaAdapter) Timeout() error {
	return a.fire(func(v atvaView) error { return os.WriteFile(filepath.Join(a.expire, v.askID), nil, 0o644) })
}

func (a *atvaAdapter) CancelByStop() error {
	return a.fire(func(atvaView) error { return os.WriteFile(a.file("cancel"), nil, 0o644) })
}

func (a *atvaAdapter) GiveUp() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["call"] == "fired") {
		return nil
	}
	return a.let("giveup", "the call to return", func() bool { return a.has("ret.held") })
}

func (a *atvaAdapter) Receive() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["call"] == "open" && v.st["chan"] != "") {
		return nil
	}
	return a.let("recv", "the select to take the channel", func() bool { return a.has("ret.held") || a.has("store.held") })
}

func (a *atvaAdapter) store(fail bool) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["call"] == "got") {
		return nil
	}
	if fail && !a.storeFailsStores {
		// A directory where the keychain file goes: the write fails.
		if err := os.Mkdir(filepath.Join(a.keychain, keychainFile()), 0o755); err != nil {
			return err
		}
	}
	return a.let("store", "the call to return", func() bool { return a.has("ret.held") })
}

func (a *atvaAdapter) Store() error      { return a.store(false) }
func (a *atvaAdapter) StoreFails() error { return a.store(true) }

func (a *atvaAdapter) ChildEnd() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	c := v.st["call"]
	if !a.gate.pass(v.st["end"] == "" && (c == "answered" || c == "failed")) {
		return nil
	}
	return a.let("ret", "the call's end to reach serve", func() bool { return a.has("scanend.held") })
}

func (a *atvaAdapter) ScanEnd() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["end"] == "out") {
		return nil
	}
	return a.let("scanend", "serve to read the end", func() bool { return a.has("scanend.done") })
}

// Poll is the page's list poll: it shows the question while the row
// has one.
func (a *atvaAdapter) Poll() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	open := v.st["kind"] != "" && v.st["hist"] == false && v.st["end"] == "" && v.st["eof"] == false
	if !a.gate.pass(a.shown != open) {
		return nil
	}
	a.shown = v.row.Ask != nil
	if a.shown {
		a.shownID = v.row.Ask.ID
	}
	return nil
}

func (a *atvaAdapter) UserAnswer() error {
	if !a.gate.pass(a.shown && a.post == "") {
		return nil
	}
	a.n++
	v, err := a.view()
	if err != nil {
		return err
	}
	a.postText = fmt.Sprintf("ans-%d-late", a.n)
	if v.st["kind"] == "secret" {
		a.postText = fmt.Sprintf("S3CR3T-%d", a.n)
	}
	a.post = "sent"
	return nil
}

// ServeWrite is serve handling the POST: 200 once the line is in the
// child's stdin, 409 otherwise.
func (a *atvaAdapter) ServeWrite() error {
	if !a.gate.pass(a.post == "sent") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := (&askAnswerAdapter{s: a.s, id: a.id}).post(ctx, "answer", map[string]string{"text": a.postText, "ask": a.shownID})
	switch {
	case err != nil:
		return err
	case code == http.StatusConflict:
		a.post = "refused"
		return nil
	case code != http.StatusOK:
		return fmt.Errorf("answer: %d %s", code, msg)
	}
	a.post = "ok"
	in := fmt.Sprintf("in.%d", a.read)
	return atvaWait("the answer to reach the child's reader", func() bool { return a.has(in + ".held") })
}

func (a *atvaAdapter) ReadLine() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["line"] == true) {
		return nil
	}
	in := fmt.Sprintf("in.%d", a.read)
	if err := a.let(in, "the child to route the line", func() bool { return a.has(in + ".done") }); err != nil {
		return err
	}
	a.read++
	if v.st["hlAsk"] == true {
		return nil
	}
	// No question open: the line went to the loop, which records it on
	// its own goroutine. A line that never lands is left for the state
	// check to report.
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if w, err := a.view(); err == nil && strings.Contains(w.st["lost"].(string), "steer") {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

// StdinEOF: serve drops the child and closes its stdin; the child fails
// the open question (hlCancelAsk).
func (a *atvaAdapter) StdinEOF() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.st["kind"] != "" && v.st["eof"] == false && v.st["line"] == false) {
		return nil
	}
	if err := os.WriteFile(a.file("eof"), nil, 0o644); err != nil {
		return err
	}
	return atvaWait("the child to see its stdin end", func() bool { return a.has("eof.done") && a.note("stdin") == "eof" })
}

func atvaCounted(name string, f func(*atvaAdapter) error) func(*atvaAdapter) error {
	return func(a *atvaAdapter) error {
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return err
	}
}

var atvaActions = map[string]func(*atvaAdapter) error{
	"AskPlain":     atvaCounted("AskPlain", (*atvaAdapter).AskPlain),
	"AskSecret":    atvaCounted("AskSecret", (*atvaAdapter).AskSecret),
	"Timeout":      atvaCounted("Timeout", (*atvaAdapter).Timeout),
	"CancelByStop": atvaCounted("CancelByStop", (*atvaAdapter).CancelByStop),
	"GiveUp":       atvaCounted("GiveUp", (*atvaAdapter).GiveUp),
	"Receive":      atvaCounted("Receive", (*atvaAdapter).Receive),
	"Store":        atvaCounted("Store", (*atvaAdapter).Store),
	"StoreFails":   atvaCounted("StoreFails", (*atvaAdapter).StoreFails),
	"ChildEnd":     atvaCounted("ChildEnd", (*atvaAdapter).ChildEnd),
	"ScanEnd":      atvaCounted("ScanEnd", (*atvaAdapter).ScanEnd),
	"Poll":         atvaCounted("Poll", (*atvaAdapter).Poll),
	"UserAnswer":   atvaCounted("UserAnswer", (*atvaAdapter).UserAnswer),
	"ServeWrite":   atvaCounted("ServeWrite", (*atvaAdapter).ServeWrite),
	"ReadLine":     atvaCounted("ReadLine", (*atvaAdapter).ReadLine),
	"StdinEOF":     atvaCounted("StdinEOF", (*atvaAdapter).StdinEOF),
	// fizz links a state with nothing enabled to itself as "end": the
	// walk stops there, and the state after it must still be the same.
	"end": func(*atvaAdapter) error { return nil },
}

// atvaPaths is the walks over testdata/ask_timeout_vs_answer's graph.
func atvaPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("ask_timeout_vs_answer", cover)
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

// walkAtvaPath drives one path and compares the whole state after every
// step; the first difference ends it.
func walkAtvaPath(a *atvaAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		var diff []string
		for k, want := range path[i].State {
			field, ok := strings.CutPrefix(k, "Ask#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(got[field]) != fmt.Sprint(want) {
				diff = append(diff, fmt.Sprintf("%s = %v, want %v", field, got[field], want))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			gb, _ := json.Marshal(got)
			return fmt.Errorf("step %d (%s): %s\n  got  %s", i, path[i].Action, strings.Join(diff, "; "), gb)
		}
		return nil
	}
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	if err := check(0); err != nil {
		return err
	}
	for i := 1; i < len(path); i++ {
		name := strings.TrimPrefix(path[i].Action, "Ask#0.")
		fn, ok := atvaActions[name]
		if !ok {
			return fmt.Errorf("step %d: no adapter action %q", i, name)
		}
		if err := fn(a); err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", i, name)
		}
		if err := check(i); err != nil {
			return err
		}
	}
	return nil
}

func actionsOf(path []tracecheck.Step) []string {
	var acts []string
	for _, st := range path[1:] {
		acts = append(acts, strings.TrimPrefix(st.Action, "Ask#0."))
	}
	return acts
}

// TestAskTimeoutVsAnswerPaths walks every generated path against a real
// serve, four serves sharing them, then replays each transcript on the
// graph. MODEL_COVER=transitions takes every link.
func TestAskTimeoutVsAnswerPaths(t *testing.T) {
	t.Parallel()
	paths := atvaPaths(t, envCover())
	const shards = 4
	g := loadAtvaGraph(t)
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newAtvaAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkAtvaPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v", i, actionsOf(paths[i]), err)
				}
			}
			t.Logf("steps taken: %v", a.did)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), atvaHistory)
			}
		})
	}
}

// The walk proves nothing unless a wrongly wired adapter fails it. The
// bug shows on one transition, StoreFails, so every walk that takes it
// is run.
func TestAskTimeoutVsAnswerPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newAtvaAdapter(t)
	a.storeFailsStores = true
	walked, failed := 0, 0
	for _, p := range atvaPaths(t, tracecheck.CoverTransitions) {
		if !slices.Contains(actionsOf(p), "StoreFails") {
			continue
		}
		walked++
		if err := walkAtvaPath(a, p); err != nil && strings.Contains(err.Error(), "(Ask#0.StoreFails)") {
			failed++
		} else {
			t.Logf("walk %v: %v", actionsOf(p), err)
		}
	}
	if walked == 0 {
		t.Fatal("no walk takes StoreFails")
	}
	if failed != walked {
		t.Fatalf("%d of %d walks through StoreFails did not fail there with a keychain that stores; the walk is not checking state", walked-failed, walked)
	}
}

func loadAtvaGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("ask_timeout_vs_answer")), "..", "testdata", "ask_timeout_vs_answer"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}

// atvaHistory reads the abstract trace off a transcript. History holds
// the ask, the answer the Asker took (ask/answer), a late answer that
// became a message, and the call's recorded end with its error; the
// steps between (the page's, serve's, the select's) leave nothing, so
// each recorded fact is the shortest run of steps that makes it:
//
//	ask/answer             UserAnswer, ServeWrite, ReadLine
//	end, answered          Receive (Store for a secret), ChildEnd
//	end, store failed      Receive, StoreFails, ChildEnd
//	end, no answer after   Timeout, GiveUp, ChildEnd
//	end, cancelled         CancelByStop, GiveUp, ChildEnd when the turn
//	                       was cancelled; else stdin ended: StdinEOF,
//	                       Receive, ChildEnd
//	a message after it     UserAnswer, ServeWrite, ReadLine
//
// The check is on kind and hist.
func atvaHistory(entries []history.Entry) []tracecheck.Step {
	st := func(kind string, hist bool) map[string]any {
		return map[string]any{"Ask#0.kind": kind, "Ask#0.hist": hist}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("", false)}}
	kind, hist, ended, eof := "", false, false, false
	add := func(actions ...string) {
		for _, a := range actions {
			steps = append(steps, tracecheck.Step{Action: "Ask#0." + a, State: st(kind, hist)})
		}
	}
	cancelled := false
	for _, e := range entries {
		if e.Kind == "cancelled" {
			cancelled = true
		}
	}
	askID := ""
	for i, e := range entries {
		switch e.Kind {
		case "ask":
			askID = str(e.Data["id"])
			kind = "plain"
			if s, _ := e.Data["secret"].(bool); s {
				kind = "secret"
			}
			if kind == "plain" {
				add("AskPlain")
			} else {
				add("AskSecret")
			}
		case "ask/answer":
			if str(e.Data["id"]) == askID {
				add("UserAnswer", "ServeWrite")
				hist = true
				add("ReadLine")
			}
		case "call":
			t := str(e.Data["tool"])
			if kind == "" || ended || (t != "ask" && t != "secret") {
				continue
			}
			ended = true
			msg := str(e.Data["error"])
			switch {
			case msg == "" && kind == "secret":
				add("Receive", "Store", "ChildEnd")
			case msg == "":
				add("Receive", "ChildEnd")
			case strings.Contains(msg, "store failed"):
				add("Receive", "StoreFails", "ChildEnd")
			case strings.Contains(msg, "no answer after"):
				add("Timeout", "GiveUp", "ChildEnd")
			case cancelled:
				add("CancelByStop", "GiveUp", "ChildEnd")
			default:
				eof = true
				add("StdinEOF", "Receive", "ChildEnd")
			}
		default:
			if t := str(e.Data["text"]); i > 0 && ended && !eof && (e.Kind == "input" || e.Kind == "steer") &&
				(strings.Contains(t, "-late") || strings.Contains(t, "S3CR3T-")) {
				add("UserAnswer", "ServeWrite", "ReadLine")
			}
		}
	}
	return steps
}

func init() { historyProjections["ask_timeout_vs_answer"] = atvaHistory }
