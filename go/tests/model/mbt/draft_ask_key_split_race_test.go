//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/draft_ask_key_split_race.fizz against a real serve: the thread
// composer's two localStorage keys for one session — draftKey (the free
// text) and askKey (which ask that text answers). askKey only follows
// the row's armed question while the composer is blank; once typing
// starts it freezes at whatever ask was open then (app.tsx ~4181), so it
// can outlive that ask being answered elsewhere and a different ask can
// open before the draft is sent or cleared. Sending must be gated on the
// exact ask the draft was frozen against.
//
// The composer's text and its frozen binding have no server record, so
// the adapter plays them the way the spec's Draft role has the page do
// it. What only the server decides is read off it: whether an ask is
// armed and which ask it is, by the transcript's ask entries; sending an
// answer goes out for the ask id the draft was frozen against, and a
// server that took a stale one for the ask now armed would show up as a
// mismatch (askKeyOf in app.tsx already needs an ask's exact identity to
// tell two asks apart, not merely that one is open).
type draftAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	walk int
	id   string
	ids  []string
	turn int // llm-control turn names, unique across walks

	hasText bool
	boundID string // "" = unbound; else the ask id the draft answers

	stats map[string]int

	// sendCurrentAsk is the deliberate bug the wrong-adapter tests
	// inject: Send answers whatever ask is armed now, not the one the
	// draft was frozen against.
	sendCurrentAsk bool
}

func newDraftAdapter(t *testing.T) *draftAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &draftAdapter{t: t, s: s, dir: control.Dir(s.Home), stats: map[string]int{}}
}

// Init starts each walk on a fresh session whose first turn is running,
// its model request held, composer blank and unbound.
func (a *draftAdapter) Init() error {
	a.walk++
	a.hasText, a.boundID = false, ""
	a.gate.reset()
	a.topUp()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), fmt.Sprintf("walk %d prompt", a.walk))
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	return a.running("the first turn to run")
}

// Cleanup archives the walk's session, which kills its child: a child
// left alive would take the next walk's queued turns.
func (a *draftAdapter) Cleanup() error {
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

func (a *draftAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Draft", Index: 0}: a}, nil
}

// draftMsgRe marks the walk and the ask ordinal an answer text carries.
var draftMsgRe = regexp.MustCompile(`^walk (\d+) answer (\d+)\b`)

// draftView is what the server says for one step: the asks it has armed
// so far, in the order they were asked, and whichever is armed now.
type draftView struct {
	row  serve.Row
	asks []string
}

func (a *draftAdapter) view() (draftView, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return draftView{}, err
	}
	v := draftView{row: row}
	for _, l := range lines {
		if l.Kind == "ask" {
			v.asks = append(v.asks, str(l.Data["id"]))
		}
	}
	return v, nil
}

// ordinal is the spec's askSeq for an ask id: its place among the
// questions asked so far, 0 for none, -1 for an id the transcript never
// asked (a mismatch).
func (v draftView) ordinal(id string) int {
	if id == "" {
		return 0
	}
	for i, x := range v.asks {
		if x == id {
			return i + 1
		}
	}
	return -1
}

func (v draftView) armed() string {
	if v.row.Ask == nil {
		return ""
	}
	return v.row.Ask.ID
}

func (v draftView) askOpen() bool {
	return v.row.Status == serve.StatusNeedsYou && v.row.Ask != nil
}

func (a *draftAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"askOpen":  v.askOpen(),
		"askSeq":   v.ordinal(v.armed()),
		"hasText":  a.hasText,
		"boundSeq": v.ordinal(a.boundID),
	}, nil
}

// Ask: the held model request asks a question. If the composer is
// blank, its binding follows the newly armed ask, as app.tsx reads
// row.ask while blank.
func (a *draftAdapter) Ask() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(!v.askOpen()) {
		return nil
	}
	a.releaseAll(&control.Turn{Call: &control.Call{Name: "ask", Args: map[string]any{
		"question": fmt.Sprintf("walk %d question %d", a.walk, len(v.asks)+1)}}})
	nv, _, err := a.wait("the question", func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusNeedsYou && r.Ask != nil
	})
	if err != nil {
		return err
	}
	if !a.hasText {
		a.boundID = nv.Ask.ID
	}
	return nil
}

// AskResolved: the armed ask is answered from another tab, or expires
// server-side. A blank composer's binding clears with it.
func (a *draftAdapter) AskResolved() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(v.askOpen()) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.answer(ctx, fmt.Sprintf("walk %d resolved elsewhere", a.walk), v.armed()); err != nil {
		return err
	}
	if err := a.running("the turn to run on after the ask resolves"); err != nil {
		return err
	}
	if !a.hasText {
		a.boundID = ""
	}
	return nil
}

// Type is the composer's first keystroke from blank.
func (a *draftAdapter) Type() error {
	if !a.gate.pass(!a.hasText) {
		return nil
	}
	a.hasText = true
	return nil
}

// Clear empties the composer. Open, it rebinds to whatever ask is now
// armed (a fresh blank composer tracks it); closed, it unbinds.
func (a *draftAdapter) Clear() error {
	if !a.gate.pass(a.hasText) {
		return nil
	}
	a.hasText = false
	v, err := a.view()
	if err != nil {
		return err
	}
	if v.askOpen() {
		a.boundID = v.armed()
	} else {
		a.boundID = ""
	}
	return nil
}

// Send answers the ask the draft was frozen against — the exact id, not
// merely that some ask is open — and requires the freeze still matches
// what is armed now.
func (a *draftAdapter) Send() error {
	v, err := a.view()
	if err != nil {
		return err
	}
	enabled := a.hasText && v.askOpen() && a.boundID == v.armed()
	if !a.gate.pass(enabled) {
		return nil
	}
	ordinal := v.ordinal(a.boundID)
	text := fmt.Sprintf("walk %d answer %d", a.walk, ordinal)
	target := a.boundID
	if a.sendCurrentAsk {
		target = v.armed()
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.answer(ctx, text, target); err != nil {
		var api *servetest.APIError
		if errors.As(err, &api) {
			// A stale binding: the server refused it, which is what the
			// wrong-adapter bug is meant to hit. Report the state as the
			// spec has it (the send did not happen) so the mismatch, not
			// this refusal, is what fails the run.
			a.hasText, a.boundID = false, ""
			return nil
		}
		return err
	}
	if err := a.running("the turn to run on after the answer"); err != nil {
		return err
	}
	a.hasText, a.boundID = false, ""
	return nil
}

// ---- helpers ----

// answer is POST /api/sessions/{id}/answer naming the question.
func (a *draftAdapter) answer(ctx context.Context, text, ask string) error {
	b, _ := json.Marshal(map[string]string{"text": text, "ask": ask})
	_, err := a.post(ctx, "/api/sessions/"+url.PathEscape(a.id)+"/answer", "application/json", bytes.NewReader(b))
	return err
}

func (a *draftAdapter) post(ctx context.Context, path, contentType string, body *bytes.Reader) ([]byte, error) {
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
	raw := new(bytes.Buffer)
	raw.ReadFrom(resp.Body)
	if resp.StatusCode/100 != 2 {
		return raw.Bytes(), &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(raw.String())}
	}
	return raw.Bytes(), nil
}

// running waits for the turn to run with its next model request held.
func (a *draftAdapter) running(what string) error {
	_, _, err := a.wait(what, func(r serve.Row, _ []serve.Line) bool {
		return r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	return err
}

func (a *draftAdapter) wait(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var row serve.Row
	var lines []serve.Line
	for {
		r, ls, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			row, lines = r, ls
			if ok(r, ls) {
				if r.Ask != nil {
					row.Ask = r.Ask
				}
				return row, lines, nil
			}
		}
		select {
		case <-ctx.Done():
			return row, lines, fmt.Errorf("waiting for %s: last status %q", what, row.Status)
		default:
		}
	}
}

// topUp keeps three block turns queued, so every model request the
// session makes is held until the adapter answers it.
func (a *draftAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turn++
		control.Queue(a.t, a.dir, fmt.Sprintf("q%08d", a.turn), control.Turn{Mode: "block", Text: "answered"})
	}
}

// held lists the requests in flight: turns taken and not yet released.
func (a *draftAdapter) held() []string {
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

func (a *draftAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

var draftActions = map[string]map[string]fmbt.ActionFunc{"Draft": {
	"Ask":         action((*draftAdapter).Ask),
	"AskResolved": action((*draftAdapter).AskResolved),
	"Type":        action((*draftAdapter).Type),
	"Clear":       action((*draftAdapter).Clear),
	"Send":        action((*draftAdapter).Send),
}}

// Each step is a real turn through a real serve, so a walk is short.
func draftOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// draftHistory reads the abstract trace off a transcript: an ask entry
// is Ask, an ask/answer whose text carries this walk's marker is Send
// (preceded by the Type that put text in the composer), and one without
// a marker is AskResolved (answered elsewhere).
func draftHistory(entries []history.Entry) []tracecheck.Step {
	hasText, askOpen, askSeq, boundSeq := false, false, 0, 0
	state := func() map[string]any {
		return map[string]any{
			"Draft#0.hasText":  hasText,
			"Draft#0.askOpen":  askOpen,
			"Draft#0.askSeq":   askSeq,
			"Draft#0.boundSeq": boundSeq,
		}
	}
	var steps []tracecheck.Step
	add := func(action string) {
		steps = append(steps, tracecheck.Step{Action: "Draft#0." + action, State: state()})
	}
	first := true
	for _, e := range entries {
		switch e.Kind {
		case "ask":
			if first {
				first = false
				steps = append(steps, tracecheck.Step{Action: "Init", State: state()})
			}
			askOpen, askSeq = true, askSeq+1
			if !hasText {
				boundSeq = askSeq
			}
			add("Ask")
		case "ask/answer":
			if first {
				first = false
				steps = append(steps, tracecheck.Step{Action: "Init", State: state()})
			}
			text, _ := e.Data["text"].(string)
			m := draftMsgRe.FindStringSubmatch(strings.TrimSpace(text))
			if m == nil {
				askOpen, askSeq = false, 0
				if !hasText {
					boundSeq = 0
				}
				add("AskResolved")
				continue
			}
			if !hasText {
				hasText = true
				add("Type")
			}
			hasText, askOpen, askSeq, boundSeq = false, false, 0, 0
			add("Send")
		}
	}
	if first {
		steps = append(steps, tracecheck.Step{Action: "Init", State: state()})
	}
	return steps
}

func init() { historyProjections["draft_ask_key_split_race"] = draftHistory }

func TestDraftAskKeySplitRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newDraftAdapter(t)
	if err := runMBT(t, "draft_ask_key_split_race", a, draftActions, draftOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	g, err := tracecheck.Load(fizzCheck(t, "draft_ask_key_split_race"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), draftHistory)
	}
}

// A run whose Send ignores the freeze and answers whatever ask is armed
// now must fail: the server rejects the stale id, or the send lands on
// the wrong question, either way a state the spec forbids.
func TestDraftAskKeySplitRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newDraftAdapter(t)
	a.sendCurrentAsk = true
	if err := runMBT(t, "draft_ask_key_split_race", a, draftActions, draftOptions()); err == nil {
		t.Fatal("a run whose Send ignores the frozen ask passed; the runner is not checking state")
	}
}
