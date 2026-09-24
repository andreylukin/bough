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
	"sync"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/notice_during_open_ask.fizz: system lines (a background agent's
// report, a watcher's wake, the page's /model) meeting an open tools.ask
// or tools.secret, driven through a real serve with llm-control as the
// model.
//
// Notify is POST /notify, the supervisor's Notify: the same {"notice"}
// line children.go report writes when a background agent's turn closes,
// without running a second agent for it. The watcher is the real
// watch.Engine (queue, dedupe, Idle-then-Wake, a refused wake marked
// failing) run in the test, so its poll and its deliver are separate
// steps and the gap between Idle and Wake can be held open; its Waker
// and Idler are serve's supWaker over HTTP: Wake is Send (POST /prompt
// adds only the same pending-ask refusal in front of it) and Idle reads
// the row's derived status, as supWaker reads StatusOf.
//
// Markers say which line ended up where: NOTE- is the report, PHANTOM-
// a person's answer that parses as {"notice"}, S3CR3T- the person's
// secret, "[watcher]" the wake and ndaModel the model command.

const (
	ndaWakePrefix = "[watcher] "
	ndaModel      = "nda-model-x"
)

type ndaAdapter struct {
	t        *testing.T
	s        *servetest.Server
	dir      string // llm-control's queue
	expire   string // BOUGH_TEST_ASK_EXPIRE_DIR
	keychain string // BOUGH_TEST_KEYCHAIN_DIR
	gate     gate

	id   string
	cwd  string
	turn int
	n    int
	held string // the model request held in flight, "" when none
	next string // the block turn queued for the next request
	ids  []string
	did  map[string]int

	// The walk's own bookkeeping, for what the server does not keep.
	note       string          // the report's marker, "" before Notify
	noticeSeen bool            // a model request was taken after the report landed
	mine       map[string]bool // ask ids the adapter answered or timed out
	model      string          // "", "refused": the last /model the server refused
	wake       string          // none | queued | checked | sent
	wakeText   string
	steered    bool  // a wake steered the turn and waits for the request's boundary
	stray      error // a second request in flight: the engine broke its own contract

	eng      *watch.Engine
	clock    time.Time
	news     string
	busy     bool // the Idler says busy: WatcherQueue polls without delivering
	wakeIn   chan struct{}
	wakeGo   chan struct{}
	tickDone chan struct{}
	wakeCode int

	// timeoutStoresSecret is the deliberate bug the wrong-adapter test
	// injects: a secret's Timeout answers it instead, so a secret gets
	// stored where the spec has none.
	timeoutStoresSecret bool
}

func newNDAAdapter(t *testing.T) *ndaAdapter {
	expire, keychain := t.TempDir(), t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Files:  map[string]string{".bough/projects/" + askProject + "/project.yml": "name: " + askProject + "\n"},
		Env:    []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire, "BOUGH_TEST_KEYCHAIN_DIR=" + keychain},
	})
	return &ndaAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire, keychain: keychain, did: map[string]int{}}
}

func (a *ndaAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("n%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

// absorb notices a model request the engine made on its own (a notice
// or an answer's result going out) and holds it as the adapter's.
func (a *ndaAdapter) absorb() bool {
	if _, err := os.Stat(filepath.Join(a.dir, a.next+".taken")); err != nil {
		return false
	}
	if a.held != "" && a.stray == nil {
		a.stray = fmt.Errorf("request %s taken while %s was still in flight", a.next, a.held)
	}
	a.held = a.next
	a.queueNext()
	if a.note != "" && a.landed(a.entries()) > 0 {
		a.noticeSeen = true
	}
	return true
}

// waitRequest waits for the engine to make its next model request.
func (a *ndaAdapter) waitRequest(why string) error {
	deadline := time.Now().Add(actionTimeout)
	for !a.absorb() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for the model request after %s: %s not taken after %s", why, a.next, actionTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil
}

func (a *ndaAdapter) entries() []history.Entry {
	es, _ := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	return es
}

// landed counts the report's arrivals in the loop: a "job" note inside
// an open turn, or the input of the turn it woke an idle agent with.
func (a *ndaAdapter) landed(es []history.Entry) int {
	n := 0
	for _, e := range es {
		if (e.Kind == "job" || (e.Kind == "input" && e.Data["reason"] == "notice")) && a.note != "" && strings.Contains(str(e.Data["text"]), a.note) {
			n++
		}
	}
	return n
}

// waitHistory waits for an entry past the first n that ok accepts.
func (a *ndaAdapter) waitHistory(n int, what string, ok func(history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		es := a.entries()
		for i := n; i < len(es); i++ {
			if ok(es[i]) {
				return nil
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("waiting for %s: not in history after %s", what, actionTimeout)
}

func (a *ndaAdapter) Init() error {
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
	a.ids = append(a.ids, row.ID)
	a.note, a.noticeSeen, a.mine, a.model, a.stray = "", false, map[string]bool{}, "", nil
	a.wake, a.wakeText, a.wakeCode, a.steered = "none", "", 0, false
	if err := a.newEngine(); err != nil {
		return err
	}
	a.gate.reset()
	if err := a.waitRequest("the first prompt"); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// newEngine loads one watcher, due at once, that polls a.news every hour
// of a clock that only the adapter moves.
func (a *ndaAdapter) newEngine() error {
	d := filepath.Join(a.s.Root, "watchers", a.id)
	if err := os.MkdirAll(d, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(d, "news.js"), []byte("// test watcher\n"), 0o644); err != nil {
		return err
	}
	a.clock = time.Unix(1_800_000_000, 0)
	a.news, a.busy = "", true
	a.wakeIn, a.wakeGo, a.tickDone = make(chan struct{}, 1), make(chan struct{}), nil
	a.eng = &watch.Engine{
		Dir: d, Session: a.id,
		Eval: ndaEval{}, Exec: ndaExec{a}, Wake: ndaWaker{a}, Busy: ndaWaker{a},
		Now:    func() time.Time { return a.clock },
		MinGap: time.Millisecond,
	}
	return a.eng.Load(context.Background())
}

// ndaEval is the watcher's file: poll "news" hourly, wake on a change.
type ndaEval struct{}

func (ndaEval) RunHook(_ context.Context, _ string, ev map[string]any) (map[string]any, error) {
	if ev["phase"] == "config" {
		return map[string]any{"every": "1h", "run": "news"}, nil
	}
	now, _ := ev["now"].(string)
	if now == "" || now == ev["prev"] {
		return map[string]any{}, nil
	}
	return map[string]any{"wake": now}, nil
}

type ndaExec struct{ a *ndaAdapter }

func (e ndaExec) Run(context.Context, string) (string, error) { return e.a.news, nil }

// ndaWaker is supWaker over HTTP. Wake holds between being called and
// sending until the adapter lets it go: that is WatcherCheck, then
// WatcherSend.
type ndaWaker struct{ a *ndaAdapter }

func (w ndaWaker) Idle(string) bool {
	if w.a.busy {
		return false
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := w.a.s.GetSession(ctx, w.a.id)
	return err == nil && row.Status != serve.StatusRunning && row.Status != serve.StatusNeedsYou
}

func (w ndaWaker) Wake(_, text string) error {
	w.a.wakeIn <- struct{}{}
	<-w.a.wakeGo
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := w.a.post(ctx, "prompt", map[string]string{"text": ndaWakePrefix + text})
	w.a.wakeCode = code
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("wake: %d %s", code, msg)
	}
	return nil
}

func (a *ndaAdapter) Cleanup() error {
	if a.tickDone != nil {
		// A walk that ended between WatcherCheck and WatcherSend.
		close(a.wakeGo)
		<-a.tickDone
		a.tickDone = nil
	}
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

// kill SIGKILLs the walk's child, found by its cwd, and waits for serve
// to see it gone.
func (a *ndaAdapter) kill() error {
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

func (a *ndaAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *ndaAdapter) row() (serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	return row, err
}

func ndaAskOf(r serve.Row) string {
	switch {
	case r.Ask == nil:
		return ""
	case r.Ask.Secret:
		return "secret"
	default:
		return "plain"
	}
}

func (a *ndaAdapter) GetState() (map[string]any, error) {
	a.absorb()
	if a.stray != nil {
		return nil, a.stray
	}
	row, err := a.row()
	if err != nil {
		return nil, err
	}
	es := a.entries()
	var plainAsked, secretAsked, noticeAnswered, phantom, stray, modelApplied bool
	for _, e := range es {
		switch e.Kind {
		case "ask":
			if s, _ := e.Data["secret"].(bool); s {
				secretAsked = true
			} else {
				plainAsked = true
			}
		case "ask/answer":
			text := str(e.Data["text"])
			switch {
			case strings.Contains(text, "NOTE-"):
				noticeAnswered = true
			case strings.Contains(text, ndaWakePrefix) || strings.HasPrefix(text, "/"):
				stray = true
			case !a.mine[str(e.Data["id"])]:
				// Answered by a line the person did not send: a secret's
				// entry hides the text, so say what was sent last.
				if a.note != "" {
					noticeAnswered = true
				} else {
					stray = true
				}
			}
		case "job":
			if strings.Contains(str(e.Data["text"]), "PHANTOM-") {
				phantom = true
			}
		case "model":
			if b, _ := json.Marshal(e.Data); strings.Contains(string(b), ndaModel) {
				modelApplied = true
			}
		}
	}
	secret := ""
	if b, err := os.ReadFile(filepath.Join(a.keychain, keychainFile())); err == nil {
		v := string(b)
		switch {
		case strings.HasPrefix(v, "S3CR3T-"):
			secret = "person"
		case strings.Contains(v, "NOTE-"):
			secret = "notice"
		case strings.Contains(v, "PHANTOM-"):
			secret = "phantom"
		default:
			secret = "other:" + v
		}
	}
	notice, count := "none", a.landed(es)
	if count > 0 {
		notice = "queued"
		if a.noticeSeen {
			notice = "delivered"
		} else {
			count--
		}
	}
	wake := a.wake
	if wake == "sent" {
		st := a.eng.Status()
		switch {
		case len(st) == 1 && st[0].Failing:
			wake = "dropped"
		case len(st) == 1 && st[0].LastWoke != nil && a.wakeLanded(es):
			wake = "delivered"
		default:
			wake = fmt.Sprintf("lost(%d)", a.wakeCode)
		}
	}
	model := a.model
	switch {
	case modelApplied:
		model = "applied"
	case model == "":
		model = "none"
	}
	return map[string]any{
		"status":         string(row.Status),
		"ask":            ndaAskOf(row),
		"plainAsked":     plainAsked,
		"secretAsked":    secretAsked,
		"notice":         notice,
		"noticeCount":    count,
		"wake":           wake,
		"model":          model,
		"secret":         secret,
		"noticeAnswered": noticeAnswered,
		"phantom":        phantom,
		"strayLine":      stray,
	}, nil
}

func (a *ndaAdapter) wakeLanded(es []history.Entry) bool {
	for _, e := range es {
		if e.Kind == "input" && a.wakeText != "" && strings.Contains(str(e.Data["text"]), a.wakeText) {
			return true
		}
	}
	return false
}

func (a *ndaAdapter) post(ctx context.Context, verb string, body any) (int, string, error) {
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

// view is the spec's require inputs as the server shows them.
type ndaView struct {
	row                     serve.Row
	ask                     string
	plainAsked, secretAsked bool
	notice                  string
}

func (a *ndaAdapter) view() (ndaView, error) {
	st, err := a.GetState()
	if err != nil {
		return ndaView{}, err
	}
	row, err := a.row()
	if err != nil {
		return ndaView{}, err
	}
	return ndaView{row: row, ask: st["ask"].(string), plainAsked: st["plainAsked"].(bool), secretAsked: st["secretAsked"].(bool), notice: st["notice"].(string)}, nil
}

// settleNotice: a report waiting at the boundary of the request just
// released goes out with the engine's next request.
func (a *ndaAdapter) settleNotice(why string) error {
	if a.note != "" && !a.noticeSeen && a.landed(a.entries()) > 0 && a.held == "" {
		return a.waitRequest(why)
	}
	return nil
}

func (a *ndaAdapter) ask(tool string, args map[string]any) error {
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: tool, Args: args})
	a.held = ""
	if _, err := waitRow(a.s, a.id, "needs-you", func(r serve.Row) bool { return r.Status == serve.StatusNeedsYou }); err != nil {
		return err
	}
	if a.steered && a.held == "" {
		// A wake that steered the turn went out at this boundary: the
		// model answers it while its question stays open, so no request
		// is left in flight beside the ask (the spec has none: a report
		// then goes out at once).
		if err := a.waitRequest("the steer"); err != nil {
			return err
		}
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	a.steered = false
	return a.settleNotice("the ask")
}

func (a *ndaAdapter) AskPlain() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && v.ask == "" && !v.plainAsked) {
		return nil
	}
	return a.ask("ask", map[string]any{"question": "Which colour?", "options": []string{"red", "blue"}})
}

func (a *ndaAdapter) AskSecret() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && v.ask == "" && !v.secretAsked) {
		return nil
	}
	return a.ask("secret", map[string]any{"name": askSecret, "question": "the API token", "project": askProject})
}

func (a *ndaAdapter) Notify() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.notice == "none") {
		return nil
	}
	n0 := len(a.entries())
	a.n++
	a.note = fmt.Sprintf("NOTE-%d-%d", len(a.ids), a.n)
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "notify", map[string]string{"text": a.note})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("notify: %d %s", code, msg)
	}
	// The child routes the line: to job-notices (a job note, or a turn
	// of its own on an idle agent), or, wrongly, to the open ask.
	if err := a.waitHistory(n0, "the report reaching the loop or an ask", func(e history.Entry) bool {
		return e.Kind == "ask/answer" || ((e.Kind == "job" || e.Kind == "input") && strings.Contains(str(e.Data["text"]), a.note))
	}); err != nil {
		return err
	}
	if a.held != "" {
		return nil // at the boundary of the request in flight
	}
	// Nothing in flight: the report (or the ask it closed) makes the next
	// request.
	return a.waitRequest("the report")
}

// DeliverNotice: the model's step in flight ends, and the report goes
// out with the next request.
func (a *ndaAdapter) DeliverNotice() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.notice == "queued" && v.ask == "") {
		return nil
	}
	if a.held == "" {
		return errors.New("deliver notice: no model request is held")
	}
	control.Release(a.t, a.dir, a.held)
	a.held, a.steered = "", false
	return a.waitRequest("the step the report waited for")
}

// WatcherQueue: the watcher's command prints news; the engine polls it
// and queues a wake, the session counting as busy so nothing delivers.
func (a *ndaAdapter) WatcherQueue() error {
	if !a.gate.pass(a.wake == "none") {
		return nil
	}
	a.n++
	a.news = fmt.Sprintf("news-%d-%d", len(a.ids), a.n)
	a.wakeText = a.news
	a.busy = true
	a.eng.Tick(context.Background())
	a.wake = "queued"
	return nil
}

// WatcherCheck: deliver() finds the session idle and calls Wake, which
// holds before its Send.
func (a *ndaAdapter) WatcherCheck() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.wake == "queued" && v.row.Status == serve.StatusDone) {
		return nil
	}
	a.busy = false
	done := make(chan struct{})
	a.tickDone = done
	go func() { a.eng.Tick(context.Background()); close(done) }()
	select {
	case <-a.wakeIn:
		a.wake = "checked"
		return nil
	case <-done:
		a.tickDone = nil
		return errors.New("watcher check: the engine found the session busy")
	case <-time.After(actionTimeout):
		return errors.New("watcher check: Wake never called")
	}
}

// WatcherSend: Wake sends. Refused on an armed ask; otherwise it steers
// the turn in flight, or starts one.
func (a *ndaAdapter) WatcherSend() error {
	if !a.gate.pass(a.wake == "checked") {
		return nil
	}
	before, err := a.row()
	if err != nil {
		return err
	}
	close(a.wakeGo)
	<-a.tickDone
	a.tickDone, a.wakeGo = nil, make(chan struct{})
	a.wake = "sent"
	if a.wakeCode != http.StatusOK {
		return nil
	}
	if err := a.waitHistory(0, "the wake's input", func(e history.Entry) bool {
		return e.Kind == "input" && strings.Contains(str(e.Data["text"]), a.wakeText)
	}); err != nil {
		return err
	}
	if before.Status == serve.StatusDone {
		if err := a.waitRequest("the wake"); err != nil {
			return err
		}
		_, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
		return err
	}
	a.steered = true
	return nil
}

// SetModel is the page's model picker: POST /model.
func (a *ndaAdapter) SetModel() error {
	st, err := a.GetState()
	if err != nil {
		return err
	}
	if !a.gate.pass(st["model"] != "applied") {
		return nil
	}
	n0 := len(a.entries())
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "model", map[string]string{"model": ndaModel})
	if err != nil {
		return err
	}
	switch code {
	case http.StatusConflict:
		a.model = "refused"
		return nil
	case http.StatusOK:
		a.model = ""
		return a.waitHistory(n0, "the child applying /model", func(e history.Entry) bool {
			b, _ := json.Marshal(e.Data)
			return e.Kind == "model" && strings.Contains(string(b), ndaModel)
		})
	default:
		return fmt.Errorf("set model: %d %s", code, msg)
	}
}

// answer POSTs text to the question on screen and waits for its call to
// return.
func (a *ndaAdapter) answer(text string) error {
	row, err := a.row()
	if err != nil {
		return err
	}
	n0 := len(a.entries())
	ctx, cancel := actionCtx()
	defer cancel()
	id := row.Ask.ID
	a.mine[id] = true
	code, msg, err := a.post(ctx, "answer", map[string]string{"text": text, "ask": id})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("answer %q: %d %s", text, code, msg)
	}
	// The call's end, not the ask/answer entry: a secret is stored
	// between the two.
	if err := a.waitHistory(n0, "the ask's call ending, or what the line became", func(e history.Entry) bool {
		t := str(e.Data["tool"])
		return (e.Kind == "call" && e.Data["phase"] != "start" && (t == "ask" || t == "secret")) || (e.Kind == "job" && strings.Contains(str(e.Data["text"]), "PHANTOM-"))
	}); err != nil {
		return err
	}
	if a.held != "" {
		return nil // the call's result waits for the request in flight
	}
	return a.waitRequest("the answer")
}

func (a *ndaAdapter) Answer() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.ask != "") {
		return nil
	}
	a.n++
	if v.ask == "secret" {
		return a.answer(fmt.Sprintf("S3CR3T-%d", a.n))
	}
	return a.answer(fmt.Sprintf("ans-%d", a.n))
}

func (a *ndaAdapter) AnswerNoticeLike() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.ask == "plain") {
		return nil
	}
	a.n++
	return a.answer(fmt.Sprintf(`{"notice":"PHANTOM-%d"}`, a.n))
}

// Timeout fires the open ask's timeout (BOUGH_TEST_ASK_EXPIRE_DIR).
func (a *ndaAdapter) Timeout() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.ask != "") {
		return nil
	}
	if a.timeoutStoresSecret && v.ask == "secret" {
		a.n++
		return a.answer(fmt.Sprintf("S3CR3T-%d", a.n))
	}
	id := v.row.Ask.ID
	a.mine[id] = true
	n0 := len(a.entries())
	if err := os.WriteFile(filepath.Join(a.expire, id), nil, 0o644); err != nil {
		return err
	}
	if err := a.waitHistory(n0, "the ask's call ending", func(e history.Entry) bool {
		t := str(e.Data["tool"])
		return e.Kind == "call" && e.Data["phase"] != "start" && (t == "ask" || t == "secret")
	}); err != nil {
		return err
	}
	if a.held != "" {
		return nil
	}
	return a.waitRequest("the timeout")
}

// Finish lets the turn end: whatever lands at a request's boundary (a
// report, a steer) makes one more request, which is released too.
func (a *ndaAdapter) Finish() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && v.ask == "") {
		return nil
	}
	for {
		if a.held == "" {
			return errors.New("finish: no model request is held")
		}
		control.Release(a.t, a.dir, a.held)
		a.held, a.steered = "", false
		deadline := time.Now().Add(actionTimeout)
		for a.held == "" {
			if time.Now().After(deadline) {
				return errors.New("finish: the turn neither ended nor asked again")
			}
			if a.absorb() {
				break
			}
			row, err := a.row()
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

func (a *ndaAdapter) Prompt() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusDone) {
		return nil
	}
	a.n++
	ctx, cancel := actionCtx()
	defer cancel()
	code, msg, err := a.post(ctx, "prompt", map[string]string{"text": fmt.Sprintf("msg-%d", a.n)})
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("prompt: %d %s", code, msg)
	}
	if err := a.waitRequest("the prompt"); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

func ndaCounted(name string, f func(*ndaAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*ndaAdapter)
		err := f(a)
		if !a.gate.off {
			a.did[name]++
		}
		return nil, err
	}
}

var ndaActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"AskPlain":         ndaCounted("AskPlain", (*ndaAdapter).AskPlain),
	"AskSecret":        ndaCounted("AskSecret", (*ndaAdapter).AskSecret),
	"Notify":           ndaCounted("Notify", (*ndaAdapter).Notify),
	"DeliverNotice":    ndaCounted("DeliverNotice", (*ndaAdapter).DeliverNotice),
	"WatcherQueue":     ndaCounted("WatcherQueue", (*ndaAdapter).WatcherQueue),
	"WatcherCheck":     ndaCounted("WatcherCheck", (*ndaAdapter).WatcherCheck),
	"WatcherSend":      ndaCounted("WatcherSend", (*ndaAdapter).WatcherSend),
	"SetModel":         ndaCounted("SetModel", (*ndaAdapter).SetModel),
	"Answer":           ndaCounted("Answer", (*ndaAdapter).Answer),
	"AnswerNoticeLike": ndaCounted("AnswerNoticeLike", (*ndaAdapter).AnswerNoticeLike),
	"Timeout":          ndaCounted("Timeout", (*ndaAdapter).Timeout),
	"Finish":           ndaCounted("Finish", (*ndaAdapter).Finish),
	"Prompt":           ndaCounted("Prompt", (*ndaAdapter).Prompt),
}}

func ndaOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// ndaHistory reads the abstract trace off a transcript, checking status
// and ask. What leaves no entry (a refused /model or wake, the watcher's
// poll and check) is not in it, except the poll and check a delivered
// wake implies: they are placed after the last turn that ended before
// it, where the session was idle.
func ndaHistory(entries []history.Entry) []tracecheck.Step {
	st := func(status, ask string) map[string]any {
		return map[string]any{"Session#0.status": status, "Session#0.ask": ask}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st("running", "")}}
	add := func(action string, state map[string]any) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state})
	}
	isWake := func(e history.Entry) bool {
		return e.Kind == "input" && strings.HasPrefix(str(e.Data["text"]), ndaWakePrefix)
	}
	wakeAt, lastDone := -1, -1
	for i, e := range entries {
		if e.Kind == "done" {
			lastDone = i
		}
		if isWake(e) {
			wakeAt = i
			break
		}
	}
	status, ask, openID, first := "running", "", "", true
	answered := false
	for i, e := range entries {
		switch e.Kind {
		case "input":
			switch {
			case first:
				first = false
			case e.Data["reason"] == "notice":
				status = "running"
				add("Notify", st(status, ask))
			case isWake(e):
				status = "running"
				add("WatcherSend", st(status, ask))
			default:
				status = "running"
				add("Prompt", st(status, ask))
			}
		case "ask":
			ask, openID, answered = "plain", str(e.Data["id"]), false
			if s, _ := e.Data["secret"].(bool); s {
				ask = "secret"
			}
			status = "needs-you"
			if ask == "plain" {
				add("AskPlain", st(status, ask))
			} else {
				add("AskSecret", st(status, ask))
			}
		case "ask/answer":
			if ask != "" && str(e.Data["id"]) == openID {
				action := "Answer"
				if strings.Contains(str(e.Data["text"]), "PHANTOM-") {
					action = "AnswerNoticeLike"
				}
				ask, status, answered = "", "running", true
				add(action, st(status, ask))
			}
		case "call":
			if t := str(e.Data["tool"]); (t == "ask" || t == "secret") && e.Data["phase"] != "start" && ask != "" {
				if !answered {
					ask, status = "", "running"
					add("Timeout", st(status, ask))
				}
			}
		case "job":
			if strings.Contains(str(e.Data["text"]), "NOTE-") {
				add("Notify", st(status, ask))
			}
		case "command":
			if strings.HasPrefix(str(e.Data["text"]), "/model ") {
				add("SetModel", st(status, ask))
			}
		case "done":
			status = "done"
			add("Finish", st(status, ask))
		}
		if i == lastDone && wakeAt >= 0 {
			add("WatcherQueue", nil)
			add("WatcherCheck", st("done", ""))
		}
	}
	return steps
}

func init() { historyProjections["notice_during_open_ask"] = ndaHistory }

func TestNoticeDuringOpenAsk(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newNDAAdapter(t)
	if err := runMBT(t, "notice_during_open_ask", a, ndaActions, ndaOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps taken: %v", a.did)
	g := loadNDAGraph(t)
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), ndaHistory)
	}
}

func ndaPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("notice_during_open_ask", cover)
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

// walkNDAPath drives one path and compares the whole state after every
// step; the first difference ends the path.
func walkNDAPath(a *ndaAdapter, path []tracecheck.Step) error {
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
		fn, ok := ndaActions["Session"][name]
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

func ndaActs(p []tracecheck.Step) []string {
	var acts []string
	for _, st := range p[1:] {
		acts = append(acts, strings.TrimPrefix(st.Action, "Session#0."))
	}
	return acts
}

// TestNoticeDuringOpenAskPaths walks every generated path (every link
// under MODEL_COVER=transitions) against four serves, then replays each
// walk's transcript on the graph.
func TestNoticeDuringOpenAskPaths(t *testing.T) {
	t.Parallel()
	paths := ndaPaths(t, envCover())
	// Reaching every state does not take every action: AnswerNoticeLike
	// ends where Answer does, so the state walks never try it. Add a
	// link walk for each action they leave out.
	took := map[string]bool{}
	for _, p := range paths {
		for _, st := range p {
			took[st.Action] = true
		}
	}
	for _, p := range ndaPaths(t, tracecheck.CoverTransitions) {
		fresh := false
		for _, st := range p {
			if !took[st.Action] {
				fresh, took[st.Action] = true, true
			}
		}
		if fresh {
			paths = append(paths, p)
		}
	}
	t.Logf("%d paths", len(paths))
	g := loadNDAGraph(t)
	const shards = 4
	var mu sync.Mutex
	did := map[string]int{}
	for s := range shards {
		t.Run(fmt.Sprint(s), func(t *testing.T) {
			t.Parallel()
			a := newNDAAdapter(t)
			for i := s; i < len(paths); i += shards {
				if err := walkNDAPath(a, paths[i]); err != nil {
					t.Errorf("path %d %v: %v", i, ndaActs(paths[i]), err)
				}
			}
			mu.Lock()
			for k, v := range a.did {
				did[k] += v
			}
			mu.Unlock()
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), ndaHistory)
			}
		})
	}
	t.Cleanup(func() { t.Logf("steps taken: %v", did) })
}

// A walk whose secret Timeout stores the person's text must fail: the
// spec stores nothing when a secret times out. That is one kind of link,
// so the walks are every link's, and only those through it are run.
func TestNoticeDuringOpenAskPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newNDAAdapter(t)
	a.timeoutStoresSecret = true
	tried := 0
	for _, p := range ndaPaths(t, tracecheck.CoverTransitions) {
		through := false
		for i := 1; i < len(p); i++ {
			if p[i].Action == "Session#0.Timeout" && p[i-1].State["Session#0.ask"] == "secret" {
				through = true
			}
		}
		if !through {
			continue
		}
		tried++
		if err := walkNDAPath(a, p); err != nil {
			t.Logf("path %v failed as it should: %v", ndaActs(p), err)
			return
		}
		if tried == 5 {
			break
		}
	}
	t.Fatalf("%d paths through a secret's Timeout passed with the timeout storing a secret; the walk is not checking state", tried)
}

func loadNDAGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("notice_during_open_ask")), "..", "testdata", "notice_during_open_ask"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}
