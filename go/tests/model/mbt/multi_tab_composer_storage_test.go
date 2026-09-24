//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"strconv"
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

// specs/multi_tab_composer_storage.fizz against a real serve: two tabs
// on one session's thread sharing its draft through localStorage while
// the turn runs, asks and is answered.
//
// The tabs, their composers and the origin's localStorage have no server
// record, so the adapter plays them as the spec's contract has the page
// do it (the page does not implement it yet: it reads the three keys once
// at mount and nothing listens for `storage`; the browser stage is where
// that is red). What only the server decides is read off it, never off
// the adapter's intent: status and the armed question are the row's,
// asks are the transcript's ask entries, a message counts as sent once
// its input is in the transcript and an answer once its ask/answer is,
// and the pasted image's path is what POST /api/attachments answered.
// An answer goes out with the ask id its draft was started for, so a
// server that took it for another question would show in answers.
type multiTabComposerAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk  int
	id    string
	ids   []string
	turns int // llm-control turn names, unique across walks

	tabs [2]mtcTab
	// localStorage: bough:draft:, bough:draft-ask:, bough:draft-atts:.
	sDraft int
	sAsk   string // the question's ask id, "" for a message
	sAtt   string
	sPath  string // the image slot's path once its upload landed
	up     bool
	pasted bool
	made   int
	wrote  []int // per message, the ordinal of the question it answers

	stats map[string]int

	// answerAsMessage is the deliberate bug the wrong-adapter tests
	// inject: an answer draft is POSTed to /prompt, as a message.
	answerAsMessage bool
}

// mtcTab is one tab's Thread: whether it is open and what its composer
// holds in memory.
type mtcTab struct {
	open  bool
	draft int
	dask  string // draftAsk: the ask id the draft answers, "" = a message
	att   string // none | pending | done
	path  string
}

func newMultiTabComposerAdapter(t *testing.T) *multiTabComposerAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &multiTabComposerAdapter{t: t, s: s, dir: control.Dir(s.Home), stats: map[string]int{}}
}

// Init starts each walk on a fresh session whose first turn is running,
// its model request held, with tab A open on it and B not yet.
func (a *multiTabComposerAdapter) Init() error {
	a.walk++
	a.tabs = [2]mtcTab{{open: true, att: "none"}, {att: "none"}}
	a.sDraft, a.sAsk, a.sAtt, a.sPath = 0, "", "none", ""
	a.up, a.pasted, a.made, a.wrote = false, false, 0, nil
	a.gate.reset()
	a.topUp()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), fmt.Sprintf("walk %d first prompt", a.walk))
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	return a.running("the first turn to run")
}

// Cleanup archives the walk's session, which kills its child: a child
// left alive would take the next walk's queued turns.
func (a *multiTabComposerAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	a.releaseAll(nil)
	a.topUp()
	return err
}

func (a *multiTabComposerAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// mtcMsgRe is the marker every message and answer the tabs send carries:
// which walk, which message id.
var mtcMsgRe = regexp.MustCompile(`^walk (\d+) msg (\d+)\b`)

// mtcView is what the server says for one step.
type mtcView struct {
	row     serve.Row
	lines   []serve.Line
	asks    []string // ask ids in the order the model asked them
	sent    []int
	answers []int
}

func (a *multiTabComposerAdapter) view() (mtcView, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return mtcView{}, err
	}
	v := mtcView{row: row, lines: lines, sent: []int{}, answers: []int{}}
	first := true
	for _, l := range lines {
		switch l.Kind {
		case "ask":
			v.asks = append(v.asks, str(l.Data["id"]))
		case "input":
			if first {
				first = false
				continue
			}
			if i := mtcMarker(a.walk, l.Text); i > 0 {
				v.sent = append(v.sent, i)
			}
		case "ask/answer":
			if i := mtcMarker(a.walk, l.Text); i > 0 {
				v.answers = append(v.answers, i)
			}
		}
	}
	return v, nil
}

// mtcMarker is the message id a text carries for this walk, 0 when none.
func mtcMarker(walk int, text string) int {
	m := mtcMsgRe.FindStringSubmatch(strings.TrimSpace(text))
	if m == nil || m[1] != strconv.Itoa(walk) {
		return 0
	}
	i, _ := strconv.Atoi(m[2])
	return i
}

// ordinal is the spec's question id for an ask id: its place among the
// questions asked, 0 for none.
func (v mtcView) ordinal(id string) int {
	if id == "" {
		return 0
	}
	for i, x := range v.asks {
		if x == id {
			return i + 1
		}
	}
	return -1 // an ask id the transcript never asked: a mismatch
}

func (v mtcView) armed() string {
	if v.row.Ask == nil {
		return ""
	}
	return v.row.Ask.ID
}

func (a *multiTabComposerAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	open, draft, dask, att := []string{}, []int{}, []int{}, []string{}
	for _, tb := range a.tabs {
		o := "none"
		if tb.open {
			o = "open"
		}
		open = append(open, o)
		draft = append(draft, tb.draft)
		dask = append(dask, v.ordinal(tb.dask))
		att = append(att, tb.att)
	}
	wrote := append([]int{}, a.wrote...)
	return map[string]any{
		"open":    open,
		"draft":   draft,
		"dask":    dask,
		"att":     att,
		"s_draft": a.sDraft,
		"s_ask":   v.ordinal(a.sAsk),
		"s_att":   a.sAtt,
		"up":      a.up,
		"pasted":  a.pasted,
		"status":  string(v.row.Status),
		"ask":     v.ordinal(v.armed()),
		"asks":    len(v.asks),
		"sent":    v.sent,
		"answers": v.answers,
		"made":    a.made,
		"wrote":   wrote,
	}, nil
}

// fresh is the spec's fresh(): tab t's composer agrees with storage.
func (a *multiTabComposerAdapter) fresh(t int) bool {
	tb := a.tabs[t]
	return tb.draft == a.sDraft && tb.dask == a.sAsk && tb.att == a.sAtt
}

func (a *multiTabComposerAdapter) adopt(t int) {
	tb := &a.tabs[t]
	tb.draft, tb.dask, tb.att, tb.path = a.sDraft, a.sAsk, a.sAtt, a.sPath
}

func (a *multiTabComposerAdapter) clear(t int) {
	a.tabs[t] = mtcTab{open: true, att: "none"}
	a.sDraft, a.sAsk, a.sAtt, a.sPath = 0, "", "none", ""
}

// typeIn is the first keystroke in tab t's empty composer: draftAsk is
// the row's armed question, as the page reads row.ask.
func (a *multiTabComposerAdapter) typeIn(t int) error {
	if !a.gate.pass(a.tabs[t].open && a.fresh(t) && a.tabs[t].draft == 0 && a.made < 2) {
		return nil
	}
	v, err := a.view()
	if err != nil {
		return err
	}
	a.made++
	q := ""
	if v.row.Status == serve.StatusNeedsYou {
		q = v.armed()
	}
	a.wrote = append(a.wrote, v.ordinal(q))
	tb := &a.tabs[t]
	tb.draft, tb.dask = a.made, q
	a.sDraft, a.sAsk = a.made, q
	return nil
}

// send is Enter in tab t. An answer draft goes to /answer with the ask
// id it was started for; a message is a steer of the running turn. A
// refused POST is not an error here: nothing lands, and the state says
// so.
func (a *multiTabComposerAdapter) send(t int) error {
	tb := a.tabs[t]
	enabled := tb.open && a.fresh(t) && tb.draft != 0 && tb.att != "pending"
	var v mtcView
	if enabled {
		var err error
		if v, err = a.view(); err != nil {
			return err
		}
		if tb.dask != "" {
			enabled = v.row.Status == serve.StatusNeedsYou && v.armed() == tb.dask
		} else {
			enabled = v.row.Status != serve.StatusNeedsYou
		}
	}
	if !a.gate.pass(enabled) {
		return nil
	}
	text := fmt.Sprintf("walk %d msg %d", a.walk, tb.draft)
	if tb.att == "done" {
		text += " [Image #1: " + tb.path + "]"
	}
	a.clear(t)
	ctx, cancel := actionCtx()
	defer cancel()
	if tb.dask != "" && !a.answerAsMessage {
		if err := a.answer(ctx, text, tb.dask); err != nil {
			var api *servetest.APIError
			if errors.As(err, &api) {
				return nil
			}
			return err
		}
		return a.running("the answered turn to run on")
	}
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		var api *servetest.APIError
		if errors.As(err, &api) {
			return nil
		}
		return err
	}
	// By its marker, not its text: serve expands an image tag into the
	// path and a note telling the model to look at it.
	_, _, err := a.wait("the message's input", func(_ serve.Row, ls []serve.Line) bool {
		for _, l := range ls[min(len(v.lines), len(ls)):] {
			if l.Kind == "input" && mtcMarker(a.walk, l.Text) == tb.draft {
				return true
			}
		}
		return false
	})
	return err
}

func (a *multiTabComposerAdapter) TypeA() error { return a.typeIn(0) }
func (a *multiTabComposerAdapter) TypeB() error { return a.typeIn(1) }
func (a *multiTabComposerAdapter) SendA() error { return a.send(0) }
func (a *multiTabComposerAdapter) SendB() error { return a.send(1) }

// PasteImageA: the tag lands in A's draft with an empty slot, in memory
// and in storage; the upload is in flight until UploadDoneA.
func (a *multiTabComposerAdapter) PasteImageA() error {
	if !a.gate.pass(a.tabs[0].open && !a.pasted && a.fresh(0) && a.tabs[0].draft != 0) {
		return nil
	}
	a.pasted, a.up = true, true
	a.tabs[0].att, a.sAtt = "pending", "pending"
	return nil
}

func (a *multiTabComposerAdapter) OpenB() error {
	if !a.gate.pass(!a.tabs[1].open) {
		return nil
	}
	a.tabs[1].open = true
	a.adopt(1)
	return nil
}

func (a *multiTabComposerAdapter) sync(t int) error {
	if a.gate.pass(a.tabs[t].open && !a.fresh(t)) {
		a.adopt(t)
	}
	return nil
}

func (a *multiTabComposerAdapter) SyncA() error { return a.sync(0) }
func (a *multiTabComposerAdapter) SyncB() error { return a.sync(1) }

// UploadDoneA is the upload answering: follow() writes the server's
// path into the slot the tag left empty, in storage and in A's memory,
// wherever the draft has moved meanwhile.
func (a *multiTabComposerAdapter) UploadDoneA() error {
	if !a.gate.pass(a.up) {
		return nil
	}
	path, err := a.upload()
	if err != nil {
		return err
	}
	a.up = false
	if a.sAtt == "pending" {
		a.sAtt, a.sPath = "done", path
	}
	if a.tabs[0].att == "pending" {
		a.tabs[0].att, a.tabs[0].path = "done", path
	}
	return nil
}

// Ask: the held model request answers with a tools.ask call.
func (a *multiTabComposerAdapter) Ask() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusRunning && len(v.asks) < 2) {
		return nil
	}
	a.releaseAll(&control.Turn{Call: &control.Call{Name: "ask", Args: map[string]any{
		"question": fmt.Sprintf("walk %d question %d", a.walk, len(v.asks)+1)}}})
	_, _, err = a.wait("the question", func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusNeedsYou && r.Ask != nil
	})
	return err
}

// AnswerElsewhere: another client answers the armed question.
func (a *multiTabComposerAdapter) AnswerElsewhere() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.row.Status == serve.StatusNeedsYou && v.row.Ask != nil) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.answer(ctx, fmt.Sprintf("walk %d answered elsewhere", a.walk), v.armed()); err != nil {
		return err
	}
	return a.running("the turn to run on after the answer")
}

// ---- helpers ----

// answer is POST /api/sessions/{id}/answer naming the question.
func (a *multiTabComposerAdapter) answer(ctx context.Context, text, ask string) error {
	b, _ := json.Marshal(map[string]string{"text": text, "ask": ask})
	_, err := a.post(ctx, "/api/sessions/"+url.PathEscape(a.id)+"/answer", "application/json", bytes.NewReader(b))
	return err
}

// upload is api.attach's POST of the pasted image.
func (a *multiTabComposerAdapter) upload() (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	raw, err := a.post(ctx, "/api/attachments", "image/png", bytes.NewReader(apPNG))
	if err != nil {
		return "", err
	}
	var out struct {
		Path string `json:"path"`
	}
	if json.Unmarshal(raw, &out) != nil || out.Path == "" {
		return "", fmt.Errorf("POST /api/attachments: no path in %s", raw)
	}
	return out.Path, nil
}

func (a *multiTabComposerAdapter) post(ctx context.Context, path, contentType string, body io.Reader) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+path, body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", contentType)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return raw, &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	return raw, nil
}

// running waits for the turn to run with its next model request held.
func (a *multiTabComposerAdapter) running(what string) error {
	_, _, err := a.wait(what, func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	return err
}

func (a *multiTabComposerAdapter) wait(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var row serve.Row
	var lines []serve.Line
	for {
		r, ls, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			row, lines = r, ls
			if ok(r, ls) {
				return row, lines, nil
			}
		}
		select {
		case <-ctx.Done():
			return row, lines, fmt.Errorf("waiting for %s: last status %q, transcript %s", what, row.Status, kinds(lines))
		case <-time.After(20 * time.Millisecond):
		}
	}
}

// topUp keeps three block turns queued, so every model request the
// session makes is held until the adapter answers it.
func (a *multiTabComposerAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("q%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

// held lists the requests in flight: turns taken and not yet released.
func (a *multiTabComposerAdapter) held() []string {
	taken, _ := filepath.Glob(filepath.Join(a.dir, "*.taken"))
	var out []string
	for _, f := range taken {
		name := strings.TrimSuffix(filepath.Base(f), ".taken")
		if _, err := os.Stat(filepath.Join(a.dir, name+".release")); errors.Is(err, os.ErrNotExist) {
			out = append(out, name)
		}
	}
	return out
}

func (a *multiTabComposerAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

// multiTabComposerActions counts what each step did, so a green run can
// say how much of it was checked.
func multiTabComposerActions(a *multiTabComposerAdapter) map[string]map[string]fmbt.ActionFunc {
	table := map[string]func(*multiTabComposerAdapter) error{
		"TypeA":           (*multiTabComposerAdapter).TypeA,
		"TypeB":           (*multiTabComposerAdapter).TypeB,
		"SendA":           (*multiTabComposerAdapter).SendA,
		"SendB":           (*multiTabComposerAdapter).SendB,
		"PasteImageA":     (*multiTabComposerAdapter).PasteImageA,
		"OpenB":           (*multiTabComposerAdapter).OpenB,
		"SyncA":           (*multiTabComposerAdapter).SyncA,
		"SyncB":           (*multiTabComposerAdapter).SyncB,
		"UploadDoneA":     (*multiTabComposerAdapter).UploadDoneA,
		"Ask":             (*multiTabComposerAdapter).Ask,
		"AnswerElsewhere": (*multiTabComposerAdapter).AnswerElsewhere,
	}
	acts := map[string]map[string]fmbt.ActionFunc{"Session": {}, "": {
		// fizz links a state with nothing enabled to itself as "end"
		// (both tabs quiet, the bounds used up); nothing happens.
		"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
	}}
	for name, f := range table {
		acts["Session"][name] = func(m any, _ []fmbt.Arg) (any, error) {
			was := a.gate.off
			err := f(m.(*multiTabComposerAdapter))
			switch {
			case err != nil:
				a.stats["failed "+name]++
			case was || a.gate.off:
				a.stats["skipped"]++
			default:
				a.stats["done "+name]++
			}
			return nil, err
		}
	}
	return acts
}

func multiTabComposerOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// multiTabComposerHistory reads the abstract trace off a transcript. The
// tabs' own steps leave nothing there, so the projection takes the one
// path every transcript also is: each sent message or answer was typed
// in tab A and sent from it, an ask is Ask, and an answer without a
// message marker came from elsewhere. The check is on the fields the
// server holds.
func multiTabComposerHistory(entries []history.Entry) []tracecheck.Step {
	status, ask, asks := "running", 0, 0
	sent, answers := []int{}, []int{}
	state := func() map[string]any {
		return map[string]any{
			"Session#0.status":  status,
			"Session#0.ask":     ask,
			"Session#0.asks":    asks,
			"Session#0.sent":    append([]int{}, sent...),
			"Session#0.answers": append([]int{}, answers...),
		}
	}
	var steps []tracecheck.Step
	add := func(action string) {
		steps = append(steps, tracecheck.Step{Action: "Session#0." + action, State: state()})
	}
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		m := mtcMsgRe.FindStringSubmatch(strings.TrimSpace(text))
		switch e.Kind {
		case "input":
			if len(steps) == 0 {
				steps = append(steps, tracecheck.Step{Action: "Init", State: state()})
				continue
			}
			if m == nil {
				continue
			}
			i, _ := strconv.Atoi(m[2])
			add("TypeA")
			sent = append(sent, i)
			add("SendA")
		case "ask":
			asks++
			status, ask = "needs-you", asks
			add("Ask")
		case "ask/answer":
			status, ask = "running", 0
			if m == nil {
				add("AnswerElsewhere")
				continue
			}
			i, _ := strconv.Atoi(m[2])
			status, ask = "needs-you", asks
			add("TypeA")
			status, ask = "running", 0
			answers = append(answers, i)
			add("SendA")
		}
	}
	return steps
}

func init() { historyProjections["multi_tab_composer_storage"] = multiTabComposerHistory }

func TestMultiTabComposerStorage(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMultiTabComposerAdapter(t)
	if err := runMBT(t, "multi_tab_composer_storage", a, multiTabComposerActions(a), multiTabComposerOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	g, err := tracecheck.Load(fizzCheck(t, "multi_tab_composer_storage"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), multiTabComposerHistory)
	}
}

// A run whose answers go out as messages must fail: the answer lands as
// a steer, or is refused, and the question stays armed.
func TestMultiTabComposerStorageCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newMultiTabComposerAdapter(t)
	a.answerAsMessage = true
	if err := runMBT(t, "multi_tab_composer_storage", a, multiTabComposerActions(a), multiTabComposerOptions()); err == nil {
		t.Fatal("a run whose answers go out as messages passed; the runner is not checking state")
	}
}

// The runner's random walks rarely get past a few steps, so every walk
// derived from the graph is also driven here, comparing the whole role
// state after each step. Four serves share the walks.
func TestMultiTabComposerStoragePaths(t *testing.T) {
	t.Parallel()
	paths := loadPaths(t, "multi_tab_composer_storage")
	g := loadMultiTabComposerGraph(t)
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newMultiTabComposerAdapter(t)
			acts := multiTabComposerActions(a)
			for i := sh; i < len(paths); i += shards {
				if err := walkMultiTabComposerPath(a, acts, paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
				}
			}
			t.Logf("shard %d: %v", sh, a.stats)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), multiTabComposerHistory)
			}
		})
	}
}

// The walk proves nothing unless a wrongly wired adapter fails it. The
// bug shows on the one transition that sends an answer draft, so the
// walks are every link's; the first failing one is enough.
func TestMultiTabComposerStoragePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	b, err := pathsJSONCover("multi_tab_composer_storage", tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	a := newMultiTabComposerAdapter(t)
	a.answerAsMessage = true
	acts := multiTabComposerActions(a)
	for _, p := range f.Paths {
		if !mtcSendsAnswer(p) {
			continue
		}
		if err := walkMultiTabComposerPath(a, acts, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every walk that sends an answer passed with answers POSTed to /prompt; the walk is not checking state")
}

// mtcSendsAnswer says whether a walk has a step that sends an answer.
func mtcSendsAnswer(p genPath) bool {
	for i := 1; i < len(p.Trace); i++ {
		b, _ := p.Trace[i-1].State["Session#0.answers"].([]any)
		c, _ := p.Trace[i].State["Session#0.answers"].([]any)
		if len(c) > len(b) && strings.HasPrefix(p.Trace[i].Action, "Session#0.Send") {
			return true
		}
	}
	return false
}

// walkMultiTabComposerPath runs one walk from Init, failing on the first
// step whose action errs, is refused by the adapter's own require, or
// leaves a state other than the walk's.
func walkMultiTabComposerPath(a *multiTabComposerAdapter, acts map[string]map[string]fmbt.ActionFunc, p genPath) error {
	defer a.Cleanup()
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	var names []string
	for i, st := range p.Trace {
		if i > 0 {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			f := acts["Session"][name]
			if f == nil {
				f = acts[""][name]
			}
			if f == nil {
				return fmt.Errorf("no adapter action for %q", st.Action)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("%v: %w", names, err)
			}
			if a.gate.off {
				return fmt.Errorf("%v: the adapter refused a step the spec enables (its require disagrees)", names)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("%v: %w", names, err)
		}
		want := roleState(st.State)
		delete(want, "session")
		if !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			return fmt.Errorf("%v: state\n got %v\nwant %v", names, jsonRound(got), jsonRound(want))
		}
	}
	return nil
}

func loadMultiTabComposerGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("multi_tab_composer_storage")), "..", "testdata", "multi_tab_composer_storage"))
	if err != nil {
		t.Fatal(err)
	}
	return g
}
