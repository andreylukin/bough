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
	"strconv"
	"strings"
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

// specs/ask_answer.fizz: the model asks the person a question
// (tools.ask, then tools.secret) and the person answers from the web,
// driven through a real serve. The model's asks are llm-control call
// turns; an ask's timeout is fired on cue through
// BOUGH_TEST_ASK_EXPIRE_DIR; the secret lands in a file keychain
// (BOUGH_TEST_KEYCHAIN_DIR) for a local session naming project p1.

// The answers carry markers, so what history and the keychain end up
// holding says which POST they came from.
const (
	askProject = "p1"
	askSecret  = "TOKEN"
)

// askAnswerAdapter plays the page: draft and inflight are its composer
// and the POST it has sent; everything else is read off the server.
type askAnswerAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	expire   string // BOUGH_TEST_ASK_EXPIRE_DIR
	keychain string // BOUGH_TEST_KEYCHAIN_DIR
	gate     gate

	id   string
	cwd  string // the walk's own, so its child can be found
	turn int
	n    int    // answer and message markers, unique across walks
	held string // the model request held in flight, "" when none
	next string // the block turn queued for the next request

	draft, draftID       string // "", "msg", "q1", "q2", "sec"; the ask id it answers
	inflight, inflightID string // "", "msg", "q1", "q2", "q2nl"
	ids                  []string
	did                  map[string]int // steps taken per action, gated ones not counted

	// The deliberate bugs the wrong-adapter tests inject. plainAsSecret
	// makes AskPlain a secret, one step into a walk, where the runner's
	// short walks reach it. answerViaPrompt POSTs answers to /prompt,
	// which serve refuses while the ask is armed; only the path walk
	// gets that deep reliably.
	plainAsSecret, answerViaPrompt bool
}

func newAskAnswerAdapter(t *testing.T) *askAnswerAdapter {
	expire, keychain := t.TempDir(), t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files:  map[string]string{".bough/projects/" + askProject + "/project.yml": "name: " + askProject + "\n"},
		Env:    []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire, "BOUGH_TEST_KEYCHAIN_DIR=" + keychain},
	})
	return &askAnswerAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, keychain: keychain, did: map[string]int{}}
}

// queueNext keeps one block turn queued, so whatever request the engine
// makes next (after an ask resolves, a steer, a new turn) is held.
func (a *askAnswerAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("t%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

// takeNext waits for the queued turn to be taken and holds it.
func (a *askAnswerAdapter) takeNext() error {
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.held = a.next
	a.queueNext()
	return nil
}

// Init starts each walk on a fresh session whose first turn is running,
// its model request held.
func (a *askAnswerAdapter) Init() error {
	os.Remove(filepath.Join(a.keychain, keychainFile()))
	a.queueNext()
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", len(a.ids)))
	row, err := a.s.CreateSession(ctx, a.cwd, "start "+a.next)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	a.draft, a.draftID, a.inflight, a.inflightID = "", "", "", ""
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	if err := a.takeNext(); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// Cleanup kills the walk's child, whatever it was doing, and takes back
// the turn it had queued, so the next walk's first request is its own.
func (a *askAnswerAdapter) Cleanup() error {
	if err := a.kill(); err != nil {
		return err
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
	return nil
}

// kill SIGKILLs the session's child (a crash) and waits for serve to
// see it gone. The child is found by its cwd, which is the walk's own:
// a created session's command line does not carry its id.
func (a *askAnswerAdapter) kill() error {
	out, _ := exec.Command("lsof", "-a", "-d", "cwd", "-c", "bough", "-Fpn").Output()
	pid := 0
	for _, l := range strings.Split(string(out), "\n") {
		switch {
		case strings.HasPrefix(l, "p"):
			pid, _ = strconv.Atoi(l[1:])
		case strings.HasPrefix(l, "n") && l[1:] == a.cwd && pid > 0:
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
	_, err := waitRow(a.s, a.id, "the child to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

func (a *askAnswerAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// askView is what the adapter reads off the server for one step.
type askView struct {
	row     serve.Row
	entries []history.Entry
	asked   int
	open    string // the child's open ask, from history: "", "q1", "q2"
	openID  string
	lastQ   string // the question the last ask entry put
	armed   string
}

func questionOf(e history.Entry) string {
	if s, _ := e.Data["secret"].(bool); s {
		return "q2"
	}
	return "q1"
}

func (a *askAnswerAdapter) view() (askView, error) {
	var v askView
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
	// The child's blocked call, read from history independently of
	// StatusOf: an ask entry opens it; its answer, the call's end
	// (native ask or secret), a block's result or the turn's close ends
	// it. A child that is gone has nothing open.
	for _, e := range v.entries {
		switch e.Kind {
		case "ask":
			v.asked++
			v.open, v.openID = questionOf(e), str(e.Data["id"])
			v.lastQ = v.open
		case "ask/answer":
			if str(e.Data["id"]) == v.openID {
				v.open, v.openID = "", ""
			}
		case "call":
			if t := str(e.Data["tool"]); t == "ask" || t == "secret" {
				v.open, v.openID = "", ""
			}
		case "result", "done", "cancelled":
			v.open, v.openID = "", ""
		}
	}
	if !row.Live {
		v.open, v.openID = "", ""
	}
	// serve's arm, probed without touching it: an answer naming a
	// question that is not the armed one is refused as expired, and one
	// with nothing armed as no pending ask. Asks never overlap, so the
	// armed one is the last asked.
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": "", "ask": "probe-not-an-ask"})
	switch {
	case err != nil:
		return v, err
	case code == http.StatusConflict && strings.Contains(msg, "expired"):
		v.armed = v.lastQ
	case code == http.StatusConflict && strings.Contains(msg, "no pending ask"):
	default:
		return v, fmt.Errorf("arm probe: %d %s", code, msg)
	}
	return v, nil
}

func str(v any) string {
	s, _ := v.(string)
	return s
}

func keychainFile() string { return "bough%" + askProject + "%" + askSecret }

// GetState is the Session role's state. The last four fields are read
// off history and the keychain, never off the adapter's own intent.
func (a *askAnswerAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	ask := ""
	if v.row.Ask != nil {
		ask = "q1"
		if v.row.Ask.Secret {
			ask = "q2"
		}
	}
	var msgAnswered, wrong, logged, split bool
	q := map[string]string{} // ask id -> question, latest wins
	for _, e := range v.entries {
		switch e.Kind {
		case "ask":
			q[str(e.Data["id"])] = questionOf(e)
		case "ask/answer":
			text := str(e.Data["text"])
			switch {
			case strings.HasPrefix(text, "msg-"):
				msgAnswered = true
			case q[str(e.Data["id"])] == "q1" && !strings.HasPrefix(text, "ans-q1-"):
				wrong = true
			case q[str(e.Data["id"])] == "q2" && text != "[secret received]":
				wrong = true
			}
		case "input":
			if strings.Contains(str(e.Data["text"]), "-tail") {
				split = true
			}
		}
		if b, _ := json.Marshal(e.Data); strings.Contains(string(b), "S3CR3T") {
			logged = true
		}
	}
	if b, err := os.ReadFile(filepath.Join(a.keychain, keychainFile())); err == nil {
		val := string(b)
		switch {
		case strings.HasPrefix(val, "msg-"):
			msgAnswered = true
		case !strings.HasPrefix(val, "S3CR3T-"):
			wrong = true
		case strings.HasSuffix(val, "-top") || strings.Contains(val, "\n"):
			split = true
		}
	}
	return map[string]any{
		"status":       string(v.row.Status),
		"alive":        v.row.Live,
		"asked":        v.asked,
		"open":         v.open,
		"armed":        v.armed,
		"ask":          ask,
		"draft":        a.draft,
		"inflight":     a.inflight,
		"msgAnswered":  msgAnswered,
		"wrongAnswer":  wrong,
		"secretLogged": logged,
		"split":        split,
	}, nil
}

// post is a POST to /api/sessions/<id>/<verb>: its status and error text.
func (a *askAnswerAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
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

// Each action asks the gate with the spec's require first.

// ask releases the held request as a call of tool: the model asks.
func (a *askAnswerAdapter) ask(tool string, args map[string]any) error {
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: tool, Args: args})
	a.held = ""
	_, err := waitRow(a.s, a.id, "needs-you", func(r serve.Row) bool { return r.Status == serve.StatusNeedsYou })
	return err
}

func (a *askAnswerAdapter) AskPlain() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.row.Status == serve.StatusRunning && v.open == "" && v.asked == 0) {
		return nil
	}
	if a.plainAsSecret {
		return a.ask("secret", map[string]any{"name": askSecret, "question": "the API token", "project": askProject})
	}
	return a.ask("ask", map[string]any{"question": "Which colour?", "options": []string{"red", "blue"}})
}

func (a *askAnswerAdapter) AskSecret() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.row.Status == serve.StatusRunning && v.open == "" && v.asked == 1) {
		return nil
	}
	return a.ask("secret", map[string]any{"name": askSecret, "question": "the API token", "project": askProject})
}

// onScreen is the question the page shows: the row's ask.
func onScreen(r serve.Row) (string, string) {
	if r.Ask == nil {
		return "", ""
	}
	if r.Ask.Secret {
		return "q2", r.Ask.ID
	}
	return "q1", r.Ask.ID
}

func (a *askAnswerAdapter) Type() error {
	if !a.gate.pass(a.draft == "" && a.inflight == "") {
		return nil
	}
	v, err := a.view()
	if err != nil {
		return err
	}
	a.draft, a.draftID = onScreen(v.row)
	switch a.draft {
	case "":
		a.draft = "msg"
	case "q2":
		// Typed into the secret's own field, which leaves with it.
		a.draft = "sec"
	}
	return nil
}

func (a *askAnswerAdapter) UseForQuestion() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	ask, askID := onScreen(v.row)
	if !a.gate.pass(a.draft != "" && a.draft != "sec" && ask != "" && a.draft != ask) {
		return nil
	}
	a.draft, a.draftID = ask, askID
	return nil
}

func (a *askAnswerAdapter) KeepAsMessage() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	ask, _ := onScreen(v.row)
	if !a.gate.pass(a.draft != "" && a.draft != "msg" && a.draft != "sec" && a.draft != ask) {
		return nil
	}
	a.draft, a.draftID = "msg", ""
	return nil
}

func (a *askAnswerAdapter) Send() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	ask, _ := onScreen(v.row)
	if !a.gate.pass(a.inflight == "" && a.draft != "" && ((a.draft == "msg" && ask == "") || a.draft == ask || (a.draft == "sec" && ask == "q2"))) {
		return nil
	}
	a.inflight, a.inflightID = a.draft, a.draftID
	if a.draft == "sec" {
		a.inflight = "q2"
	}
	a.draft, a.draftID = "", ""
	return nil
}

func (a *askAnswerAdapter) SendMultiline() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	ask, _ := onScreen(v.row)
	if !a.gate.pass(a.inflight == "" && a.draft == "q2" && ask == "q2") {
		return nil
	}
	a.inflight, a.inflightID = "q2nl", a.draftID
	a.draft, a.draftID = "", ""
	return nil
}

// Deliver is serve handling the POST the page sent.
func (a *askAnswerAdapter) Deliver() error {
	if !a.gate.pass(a.inflight != "") {
		return nil
	}
	before, err := a.view()
	if err != nil {
		return err
	}
	a.n++
	inflight, askID := a.inflight, a.inflightID
	a.inflight, a.inflightID = "", ""
	ctx, cancel := actionCtx()
	defer cancel()
	var code int
	var msg string
	switch {
	case inflight == "msg":
		code, msg, err = a.post(ctx, "prompt", map[string]string{"text": fmt.Sprintf("msg-%d", a.n)})
	default:
		text := fmt.Sprintf("ans-q1-%d", a.n)
		switch inflight {
		case "q2":
			text = fmt.Sprintf("S3CR3T-%d", a.n)
		case "q2nl":
			text = fmt.Sprintf("S3CR3T-%d-top\nS3CR3T-%d-tail", a.n, a.n)
		}
		verb := "answer"
		if a.answerViaPrompt {
			verb = "prompt"
		}
		code, msg, err = a.post(ctx, verb, map[string]string{"text": text, "ask": askID})
	}
	if err != nil {
		return err
	}
	switch {
	case code == http.StatusConflict || code == http.StatusBadRequest:
		// Refused: the page reports it and nothing changed.
		return nil
	case code != http.StatusOK:
		return fmt.Errorf("deliver %s: %d %s", inflight, code, msg)
	}
	if inflight == "msg" {
		if before.row.Status == serve.StatusRunning && a.held != "" {
			// A steer: it lands in history now and on the model's next
			// request, which Finish drives.
			return a.waitEntries(len(before.entries), "the steer", func(e history.Entry) bool { return e.Kind == "input" })
		}
		// A new turn, on this child or a respawned one.
		if err := a.takeNext(); err != nil {
			return err
		}
		_, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
		return err
	}
	if before.open == "" {
		// A stale arm: serve wrote the line and nothing was waiting.
		return nil
	}
	// The ask's call returns and the model's next request is held.
	return a.takeNext()
}

// waitEntries waits for an entry past the first n that ok accepts.
func (a *askAnswerAdapter) waitEntries(n int, what string, ok func(history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
		for i := n; i < len(es); i++ {
			if ok(es[i]) {
				return nil
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("waiting for %s: not in history after %s", what, actionTimeout)
}

// Resolve times the open ask out, as its timeout would.
func (a *askAnswerAdapter) Resolve() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.open != "") {
		return nil
	}
	if err := os.WriteFile(filepath.Join(a.expire, v.openID), nil, 0o644); err != nil {
		return err
	}
	a.dropSecretDraft()
	return a.takeNext()
}

// Finish lets the turn end: a steer that landed meanwhile makes one more
// request, which is released too.
func (a *askAnswerAdapter) Finish() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.row.Status == serve.StatusRunning && v.open == "") {
		return nil
	}
	for {
		if a.held == "" {
			return errors.New("finish: no model request is held")
		}
		control.Release(a.t, a.dir, a.held)
		a.held = ""
		deadline := time.Now().Add(actionTimeout)
		for a.held == "" {
			if time.Now().After(deadline) {
				return errors.New("finish: the turn neither ended nor asked again")
			}
			if _, err := os.Stat(filepath.Join(a.dir, a.next+".taken")); err == nil {
				a.held = a.next
				a.queueNext()
				break
			}
			ctx, cancel := actionCtx()
			row, _, err := a.s.GetSession(ctx, a.id)
			cancel()
			if err != nil {
				return err
			}
			if row.Status == serve.StatusDone {
				return nil
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
}

func (a *askAnswerAdapter) Exit() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Live && v.row.Status == serve.StatusNeedsYou) {
		return nil
	}
	a.held = ""
	a.dropSecretDraft()
	return a.kill()
}

// dropSecretDraft: the secret field's value leaves with its question.
func (a *askAnswerAdapter) dropSecretDraft() {
	if a.draft == "sec" {
		a.draft, a.draftID = "", ""
	}
}

func waitTaken(dir, name string) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(filepath.Join(dir, name+".taken")); err == nil {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("llm-control: turn %q not taken after %s", name, actionTimeout)
}

// counted is action, also tallying the steps the gate let through: a
// green run whose walks never reached an action has not tested it.
func counted(name string, f func(*askAnswerAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*askAnswerAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var askAnswerActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"AskPlain":       counted("AskPlain", (*askAnswerAdapter).AskPlain),
	"AskSecret":      counted("AskSecret", (*askAnswerAdapter).AskSecret),
	"Type":           counted("Type", (*askAnswerAdapter).Type),
	"UseForQuestion": counted("UseForQuestion", (*askAnswerAdapter).UseForQuestion),
	"KeepAsMessage":  counted("KeepAsMessage", (*askAnswerAdapter).KeepAsMessage),
	"Send":           counted("Send", (*askAnswerAdapter).Send),
	"SendMultiline":  counted("SendMultiline", (*askAnswerAdapter).SendMultiline),
	"Deliver":        counted("Deliver", (*askAnswerAdapter).Deliver),
	"Resolve":        counted("Resolve", (*askAnswerAdapter).Resolve),
	"Finish":         counted("Finish", (*askAnswerAdapter).Finish),
	"Exit":           counted("Exit", (*askAnswerAdapter).Exit),
}}

// Most actions are the page's and cost nothing; a walk needs about ten
// to reach a second question's answer.
func askAnswerOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
}

// askAnswerHistory reads the abstract trace off a transcript. The page's
// own steps leave nothing in history, so a delivered answer or message
// is the Type, Send, Deliver that made it; a refused POST left nothing
// and changed nothing the check reads. An ask's call ending with no
// answer is Resolve; an input while a question was still open means its
// child died first (Exit). The check is on status and ask.
func askAnswerHistory(entries []history.Entry) []tracecheck.Step {
	st := func(status, ask string) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.ask": ask}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("running", "")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	deliver := func() {
		add("Type", nil)
		add("Send", nil)
		add("Deliver", st("running", ""))
	}
	open, openID, answered := "", "", false
	first := true
	for _, e := range entries {
		switch e.Kind {
		case "input":
			if first {
				first = false
				continue
			}
			if open != "" {
				add("Exit", st("interrupted", ""))
				open = ""
			}
			deliver()
		case "ask":
			open, openID, answered = questionOf(e), str(e.Data["id"]), false
			if open == "q1" {
				add("AskPlain", st("needs-you", "q1"))
			} else {
				add("AskSecret", st("needs-you", "q2"))
			}
		case "ask/answer":
			if open != "" && str(e.Data["id"]) == openID {
				deliver()
				answered = true
			}
		case "call":
			if t := str(e.Data["tool"]); (t == "ask" || t == "secret") && open != "" {
				if !answered {
					add("Resolve", st("running", ""))
				}
				open = ""
			}
		case "done":
			add("Finish", st("done", ""))
		}
	}
	return steps
}

func init() { historyProjections["ask_answer"] = askAnswerHistory }

func TestAskAnswer(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAskAnswerAdapter(t)
	if err := runMBT(t, "ask_answer", a, askAnswerActions, askAnswerOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	// The runner picks among all eleven actions, enabled or not, so its
	// walks rarely get past three steps; TestAskAnswerPaths covers the
	// deep ones.
	t.Logf("steps taken: %v", a.did)
	g, err := tracecheck.Load(fizzCheck(t, "ask_answer"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), askAnswerHistory)
	}
}

// A run whose AskPlain asks for a secret must fail: the question on
// screen is then q2 where the spec has q1.
func TestAskAnswerCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newAskAnswerAdapter(t)
	a.plainAsSecret = true
	if err := runMBT(t, "ask_answer", a, askAnswerActions, askAnswerOptions()); err == nil {
		t.Fatal("a run whose AskPlain asks for a secret passed; the runner is not checking state")
	}
}

// askAnswerPaths is testdata/ask_answer/paths.json: every path the
// generator covers, each step with the spec's whole state after it.
// TestSpecFixtures keeps it the graph of the current spec.
func askAnswerPaths(t *testing.T) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSON("ask_answer")
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

// walkAskPath drives one generated path and compares the whole state after
// every step; the first difference ends the path.
func walkAskPath(a *askAnswerAdapter, path []tracecheck.Step) error {
	defer a.Cleanup()
	check := func(i int, want map[string]any) error {
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, path[i].Action, err)
		}
		gb, _ := json.Marshal(got)
		var g map[string]any
		json.Unmarshal(gb, &g)
		for k, v := range want {
			field, ok := strings.CutPrefix(k, "Session#0.")
			if !ok {
				continue
			}
			if fmt.Sprint(g[field]) != fmt.Sprint(v) {
				return fmt.Errorf("step %d (%s): %s = %v, want %v\n  got  %s", i, path[i].Action, field, g[field], v, gb)
			}
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
		fn, ok := askAnswerActions["Session"][name]
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

// TestAskAnswerPaths walks every generated path, which the runner's
// random walks almost never get deep enough to reach. It needs no fizz
// tools: the paths are checked in. Four serves share the paths.
func TestAskAnswerPaths(t *testing.T) {
	t.Parallel()
	paths := askAnswerPaths(t)
	const shards = 4
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newAskAnswerAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkAskPath(a, paths[i]); err != nil {
					var acts []string
					for _, st := range paths[i][1:] {
						acts = append(acts, strings.TrimPrefix(st.Action, "Session#0."))
					}
					t.Errorf("path %d %v: %v", i, acts, err)
				}
			}
			t.Logf("steps taken: %v", a.did)
			for _, id := range a.ids {
				checkHistory(t, loadAskAnswerGraph(t), sessionHistory(t, a.s.Home, id), askAnswerHistory)
			}
		})
	}
}

// The path walk proves nothing unless a wrongly wired adapter fails it.
func TestAskAnswerPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newAskAnswerAdapter(t)
	a.answerViaPrompt = true
	failed := 0
	for _, p := range askAnswerPaths(t) {
		if walkAskPath(a, p) != nil {
			failed++
		}
	}
	if failed == 0 {
		t.Fatal("every path passed with answers POSTed to /prompt; the walk is not checking state")
	}
	t.Logf("%d paths failed with answers POSTed to /prompt", failed)
}

// loadAskAnswerGraph is the checked-in graph, so the trace check needs
// no fizz run either.
func loadAskAnswerGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("ask_answer")), "..", "testdata", "ask_answer"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}
