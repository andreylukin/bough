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
	"reflect"
	"slices"
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

// specs/transcript_edit_retry.fizz against a real serve: resending a
// past prompt (the transcript's Edit, the error card's Retry, the Not
// sent row's Retry and Edit, Edit on the queued message) must send and
// record what the person typed, never what the child expanded it to.
//
// The adapter plays the page. The composer, the Not-sent row and the
// queue are the page's own state, so the adapter keeps them, each as
// the text it holds and the text the person meant by it; a draft whose
// text is not what was meant is "leak". A resend takes its words where
// a client of the server has to: the input entry's data.typed (text
// when absent), which is the server's record of the words typed. What
// the server decides is read off it at every step: the row's status and
// archived flag, and per input entry its kind, how its turn ended and
// whether its recorded typed words are the ones the person meant
// (inputs). The session works in a dir with notes.txt, so "@notes.txt"
// expands to a [file:] block, and an image is a real upload whose
// "[Image #1: path]" the engine answers with its view_image sentence.
//
// The browser stage reads the same fields off the DOM, where the page's
// own resend (which text it takes from the transcript) can be wrong.
type terAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	work string
	gate gate

	walk  int
	id    string
	ids   []string
	turns int // llm-control turn names, unique across walks

	// The page's own state: each slot's text and what the person meant.
	draft, draftMeant   string
	failed, failedMeant string
	queued, queuedMeant string
	uploading           bool
	meant               []string // per turn: what the person typed for it
	status              string   // the row's, as of the last step
	archived            bool

	stats map[string]int

	// retryExpanded is the deliberate bug the wrong-adapter tests
	// inject: the error card's Retry resends the input entry's text
	// (the expansion the model was sent) instead of its typed words,
	// which is what the page's RetryTurn does with turn.prompt.text.
	retryExpanded bool
}

const terFile = "notes.txt"

func newTerAdapter(t *testing.T) *terAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	work := s.Dir(t, "work")
	if err := os.WriteFile(filepath.Join(work, terFile), []byte("plain words\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return &terAdapter{t: t, s: s, dir: control.Dir(s.Home), work: work, stats: map[string]int{}}
}

// Init opens a fresh idle session: the spec's Init.
func (a *terAdapter) Init() error {
	a.walk++
	a.draft, a.draftMeant, a.failed, a.failedMeant, a.queued, a.queuedMeant = "", "", "", "", "", ""
	a.uploading, a.meant, a.status, a.archived = false, nil, "idle", false
	a.gate.reset()
	a.topUp()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.work, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.status = pageStatus(row.Status)
	return nil
}

// Cleanup archives the walk's session, which kills its child: a child
// left alive would take the next walk's queued turns.
func (a *terAdapter) Cleanup() error {
	if a.id == "" {
		return nil
	}
	a.releaseAll(nil)
	if a.archived {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.Archive(ctx, a.id)
	a.releaseAll(nil)
	return err
}

func (a *terAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *terAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	ks, ends, inputs := []string{}, []string{}, []string{}
	for i, t := range terTurns(lines) {
		ks = append(ks, terKind(t.typed))
		ends = append(ends, t.end)
		in := "expanded"
		if i < len(a.meant) && t.typed == a.meant[i] {
			in = "typed"
		}
		inputs = append(inputs, in)
	}
	return map[string]any{
		"status":    pageStatus(row.Status),
		"kinds":     ks,
		"ends":      ends,
		"inputs":    inputs,
		"draft":     terSlot(a.draft, a.draftMeant),
		"uploading": a.uploading,
		"archived":  row.Archived,
		"failed":    terSlot(a.failed, a.failedMeant),
		"queued":    terSlot(a.queued, a.queuedMeant),
	}, nil
}

// terSlot is a composer slot as the spec names it: "", the kind of what
// the person meant, or "leak" for text nobody typed.
func terSlot(text, meant string) string {
	switch {
	case text == "":
		return ""
	case text != meant:
		return "leak"
	}
	return terKind(text)
}

// terKind is what a prompt carries: an image tag, else an @file.
func terKind(typed string) string {
	if strings.Contains(typed, "[Image #") {
		return "image"
	}
	return "rich"
}

// terTurn is one input entry: its recorded typed words and its end.
type terTurn struct {
	text   string // what the model was sent
	typed  string // data.typed, or text when absent
	end    string // running | ok | error
	failed bool   // an error entry was recorded in it
}

// terTurns reads the turns off a transcript: each input opens one, an
// error entry marks it, and done closes it.
func terTurns(lines []serve.Line) []terTurn {
	var out []terTurn
	for _, l := range lines {
		n := len(out)
		switch {
		case l.Kind == "input":
			typed, _ := l.Data["typed"].(string)
			if typed == "" {
				typed = l.Text
			}
			out = append(out, terTurn{text: l.Text, typed: typed, end: "running"})
		case n == 0 || out[n-1].end != "running":
		case l.Kind == "error":
			out[n-1].failed = true
		case l.Kind == "done" && out[n-1].failed:
			out[n-1].end = "error"
		case l.Kind == "done":
			out[n-1].end = "ok"
		}
	}
	return out
}

func (a *terAdapter) flushReady() bool {
	return a.status != "running" && a.queued != "" && len(a.meant) < 2
}

func (a *terAdapter) draftKind() string { return terSlot(a.draft, a.draftMeant) }

// ---- the person: the composer ----

func (a *terAdapter) Type() error {
	if a.gate.pass(!a.archived && a.draft == "" && !a.flushReady()) {
		a.draft = fmt.Sprintf("walk %d turn %d: read @%s", a.walk, len(a.meant)+1, terFile)
		a.draftMeant = a.draft
	}
	return nil
}

// PasteImage is the tag landing with no path yet.
func (a *terAdapter) PasteImage() error {
	if a.gate.pass(!a.archived && !a.uploading && a.draft == "" && !a.flushReady()) {
		a.draft, a.draftMeant, a.uploading = "[Image #1] ", "[Image #1] ", true
	}
	return nil
}

// UploadDone is POST /api/attachments answering: the tag takes the
// server's path, as send() swaps it in.
func (a *terAdapter) UploadDone() error {
	if !a.gate.pass(a.uploading) {
		return nil
	}
	path, err := a.upload()
	if err != nil {
		return err
	}
	a.draft = fmt.Sprintf("walk %d look at [Image #1: %s]", a.walk, path)
	a.draftMeant, a.uploading = a.draft, false
	return nil
}

func (a *terAdapter) Clear() error {
	if a.gate.pass(a.draft != "" && !a.uploading && !a.flushReady()) {
		a.draft, a.draftMeant = "", ""
	}
	return nil
}

func (a *terAdapter) Send() error {
	if !a.gate.pass(a.draft != "" && !a.uploading && !a.archived && a.status != "running" &&
		len(a.meant) < 2 && !a.flushReady()) {
		return nil
	}
	if err := a.start(a.draft, a.draftMeant); err != nil {
		return err
	}
	a.draft, a.draftMeant = "", ""
	return nil
}

// SendFails is the POST answered with an error: the adapter sends it
// with a wrong token, the one refusal a healthy serve gives, and the
// server must record nothing.
func (a *terAdapter) SendFails() error {
	if !a.gate.pass(a.draftKind() == "rich" && !a.archived && a.status != "running" &&
		a.failed == "" && len(a.meant) == 0 && !a.flushReady()) {
		return nil
	}
	before := a.newest()
	ctx, cancel := actionCtx()
	defer cancel()
	b, _ := json.Marshal(map[string]string{"text": a.draft})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/prompt", bytes.NewReader(b))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer not-the-token")
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	resp.Body.Close()
	if resp.StatusCode/100 == 2 {
		return fmt.Errorf("SendFails: a prompt with a wrong token was accepted (%s)", resp.Status)
	}
	if n := a.newest(); n != before {
		return fmt.Errorf("SendFails: a refused prompt was recorded (seq %d -> %d)", before, n)
	}
	a.failed, a.failedMeant = a.draft, a.draftMeant
	a.draft, a.draftMeant = "", ""
	return nil
}

func (a *terAdapter) Enqueue() error {
	if a.gate.pass(a.status == "running" && a.draftKind() == "rich" && a.queued == "" && len(a.meant) == 1) {
		a.queued, a.queuedMeant = a.draft, a.draftMeant
		a.draft, a.draftMeant = "", ""
	}
	return nil
}

func (a *terAdapter) Flush() error {
	if !a.gate.pass(a.flushReady()) {
		return nil
	}
	if err := a.start(a.queued, a.queuedMeant); err != nil {
		return err
	}
	a.queued, a.queuedMeant = "", ""
	return nil
}

// ---- the person: resending ----

// resendText is the words a resend takes from input entry i: its typed
// words, or with expanded the text the model was sent.
func (a *terAdapter) resendText(i int, expanded bool) (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return "", err
	}
	ts := terTurns(lines)
	if i >= len(ts) {
		return "", fmt.Errorf("no input entry %d in %s", i, kinds(lines))
	}
	if expanded {
		return ts[i].text, nil
	}
	return ts[i].typed, nil
}

func (a *terAdapter) EditPrompt(args []fmbt.Arg) error {
	if !a.gate.pass(a.draft == "" && len(a.meant) > 0 && !a.flushReady()) {
		return nil
	}
	i, ok := argOf(args, "i").(int)
	if !ok || i < 0 || i >= len(a.meant) {
		i = 0
	}
	text, err := a.resendText(i, false)
	if err != nil {
		return err
	}
	a.draft, a.draftMeant = text, a.meant[i]
	return nil
}

// errTurns is the indexes of the turns whose error card is up.
func (a *terAdapter) errTurns() []int {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, _ := a.s.GetSession(ctx, a.id)
	var out []int
	for i, t := range terTurns(lines) {
		if t.end == "error" {
			out = append(out, i)
		}
	}
	return out
}

func (a *terAdapter) RetryTurn(args []fmbt.Arg) error {
	enabled := a.status != "running" && !a.archived && len(a.meant) < 3 && !a.flushReady()
	var errs []int
	if enabled && !a.gate.off {
		errs = a.errTurns()
	}
	if !a.gate.pass(enabled && len(errs) > 0) {
		return nil
	}
	i, ok := argOf(args, "i").(int)
	if !ok || !slices.Contains(errs, i) {
		i = errs[0]
	}
	text, err := a.resendText(i, a.retryExpanded)
	if err != nil {
		return err
	}
	return a.start(text, a.meant[i])
}

func (a *terAdapter) RetryFailed() error {
	if !a.gate.pass(a.failed != "" && a.status != "running" && !a.archived && len(a.meant) < 2 && !a.flushReady()) {
		return nil
	}
	if err := a.start(a.failed, a.failedMeant); err != nil {
		return err
	}
	a.failed, a.failedMeant = "", ""
	return nil
}

func (a *terAdapter) EditFailed() error {
	if a.gate.pass(a.failed != "" && a.draft == "" && !a.flushReady()) {
		a.draft, a.draftMeant = a.failed, a.failedMeant
		a.failed, a.failedMeant = "", ""
	}
	return nil
}

func (a *terAdapter) EditQueued() error {
	if a.gate.pass(a.queued != "" && a.draft == "" && !a.flushReady()) {
		a.draft, a.draftMeant = a.queued, a.queuedMeant
		a.queued, a.queuedMeant = "", ""
	}
	return nil
}

func (a *terAdapter) Archive() error {
	if !a.gate.pass(!a.archived && a.status != "running" && a.draft == "" && a.failed == "" && !a.flushReady()) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	a.archived = true
	return nil
}

func (a *terAdapter) Unarchive() error {
	if !a.gate.pass(a.archived) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Unarchive(ctx, a.id); err != nil {
		return err
	}
	a.archived = false
	return nil
}

// ---- the model ----

// Finish answers the held request, or fails it when the runner picks
// the error branch, and waits for the turn to close.
func (a *terAdapter) Finish(args []fmbt.Arg) error {
	if !a.gate.pass(a.status == "running") {
		return nil
	}
	var release *control.Turn
	if end, _ := argOf(args, "end").(string); end == "error" {
		release = &control.Turn{Mode: "error", Error: "model says no"}
	}
	row, _, err := a.wait("the turn to end", func(r serve.Row, ls []serve.Line) bool {
		ts := terTurns(ls)
		if r.Status != serve.StatusRunning && len(ts) > 0 && ts[len(ts)-1].end != "running" {
			return true
		}
		a.releaseAll(release)
		return false
	})
	if err != nil {
		return err
	}
	a.status = pageStatus(row.Status)
	return nil
}

// ---- helpers ----

// start POSTs text as a new turn and waits for its input entry, the
// running row and the model request it holds.
func (a *terAdapter) start(text, meant string) error {
	a.topUp()
	since := a.newest()
	ctx, cancel := actionCtx()
	err := a.s.Prompt(ctx, a.id, text)
	cancel()
	if err != nil {
		return err
	}
	row, _, err := a.wait("the resent turn to run", func(r serve.Row, ls []serve.Line) bool {
		return hasKindAfter(ls, "input", since) && r.Status == serve.StatusRunning && len(a.held()) > 0
	})
	if err != nil {
		return err
	}
	a.meant = append(a.meant, meant)
	a.status = pageStatus(row.Status)
	return nil
}

func (a *terAdapter) upload() (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/attachments", bytes.NewReader(apPNG))
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "image/png")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	var out struct {
		Path string `json:"path"`
	}
	if resp.StatusCode/100 != 2 || json.Unmarshal(raw, &out) != nil || out.Path == "" {
		return "", fmt.Errorf("POST /api/attachments: %s %s", resp.Status, raw)
	}
	return out.Path, nil
}

func (a *terAdapter) newest() int64 {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil || len(lines) == 0 {
		return 0
	}
	return lines[len(lines)-1].Seq
}

func (a *terAdapter) wait(what string, ok func(serve.Row, []serve.Line) bool) (serve.Row, []serve.Line, error) {
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
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// topUp keeps three block turns queued, so every model request is held
// until the adapter answers it.
func (a *terAdapter) topUp() {
	names, _ := filepath.Glob(filepath.Join(a.dir, "*.json"))
	for n := len(names); n < 3; n++ {
		a.turns++
		control.Queue(a.t, a.dir, fmt.Sprintf("q%08d", a.turns), control.Turn{Mode: "block", Text: "answered"})
	}
}

// held lists the requests taken and not yet released.
func (a *terAdapter) held() []string {
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

func (a *terAdapter) releaseAll(turn *control.Turn) {
	for _, name := range a.held() {
		if turn == nil {
			control.Release(a.t, a.dir, name)
		} else {
			control.ReleaseWith(a.t, a.dir, name, *turn)
		}
	}
	a.topUp()
}

func terWithArgs(f func(*terAdapter, []fmbt.Arg) error) fmbt.ActionFunc {
	return func(m any, args []fmbt.Arg) (any, error) { return nil, f(m.(*terAdapter), args) }
}

// terActions counts what each step did, so a green run says how much of
// it was checked.
func terActions(a *terAdapter) map[string]map[string]fmbt.ActionFunc {
	acts := map[string]map[string]fmbt.ActionFunc{"Session": {}, "": {}}
	for role, m := range terActionTable {
		for name, f := range m {
			acts[role][name] = func(m any, args []fmbt.Arg) (any, error) {
				was := a.gate.off
				v, err := f(m, args)
				switch {
				case err != nil:
					a.stats["failed "+name]++
				case was || a.gate.off:
					a.stats["skipped"]++
				default:
					a.stats["done "+name]++
				}
				return v, err
			}
		}
	}
	return acts
}

var terActionTable = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Type":        action((*terAdapter).Type),
	"PasteImage":  action((*terAdapter).PasteImage),
	"UploadDone":  action((*terAdapter).UploadDone),
	"Clear":       action((*terAdapter).Clear),
	"Send":        action((*terAdapter).Send),
	"SendFails":   action((*terAdapter).SendFails),
	"Enqueue":     action((*terAdapter).Enqueue),
	"Flush":       action((*terAdapter).Flush),
	"EditPrompt":  terWithArgs((*terAdapter).EditPrompt),
	"RetryTurn":   terWithArgs((*terAdapter).RetryTurn),
	"RetryFailed": action((*terAdapter).RetryFailed),
	"EditFailed":  action((*terAdapter).EditFailed),
	"EditQueued":  action((*terAdapter).EditQueued),
	"Archive":     action((*terAdapter).Archive),
	"Unarchive":   action((*terAdapter).Unarchive),
	"Finish":      terWithArgs((*terAdapter).Finish),
}, "": {
	// A state with its bounds used up links to itself as "end".
	"end": func(any, []fmbt.Arg) (any, error) { return nil, nil },
}}

func terOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// terForkArgs is the choice the runner would pass for a step that forks,
// read off the path's next state: which turn Edit or Retry takes (any
// turn of the kind the next state shows), and how Finish ends.
func terForkArgs(action string, before, after map[string]any) []fmbt.Arg {
	strs := func(v any) []string {
		var out []string
		for _, x := range v.([]any) {
			out = append(out, x.(string))
		}
		return out
	}
	switch action {
	case "EditPrompt":
		for i, k := range strs(before["kinds"]) {
			if k == after["draft"] {
				return []fmbt.Arg{{Name: "i", Value: i}}
			}
		}
	case "RetryTurn":
		ks, ends, next := strs(before["kinds"]), strs(before["ends"]), strs(after["kinds"])
		for i := range ks {
			if ends[i] == "error" && ks[i] == next[len(next)-1] {
				return []fmbt.Arg{{Name: "i", Value: i}}
			}
		}
	case "Finish":
		ends := strs(after["ends"])
		return []fmbt.Arg{{Name: "end", Value: ends[len(ends)-1]}}
	}
	return nil
}

// walkTerPath runs one walk from Init, failing on the first step whose
// action errs, is refused by the adapter's own require, or leaves a
// state other than the walk's.
func walkTerPath(a *terAdapter, acts map[string]map[string]fmbt.ActionFunc, n int, p genPath) error {
	defer a.Cleanup()
	if err := a.Init(); err != nil {
		return fmt.Errorf("walk %d: Init: %w", n, err)
	}
	var names []string
	for i, st := range p.Trace {
		want := roleState(st.State)
		if i > 0 {
			name := strings.TrimPrefix(st.Action, "Session#0.")
			names = append(names, name)
			f := acts["Session"][name]
			if f == nil {
				f = acts[""][name]
			}
			if f == nil {
				return fmt.Errorf("walk %d: no adapter action for %q", n, st.Action)
			}
			before := jsonRound(roleState(p.Trace[i-1].State)).(map[string]any)
			after := jsonRound(want).(map[string]any)
			if _, err := f(a, terForkArgs(name, before, after)); err != nil {
				return fmt.Errorf("walk %d %v: %w", n, names, err)
			}
			if a.gate.off {
				return fmt.Errorf("walk %d %v: the adapter refused a step the spec enables (its require disagrees)", n, names)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("walk %d %v: %w", n, names, err)
		}
		if !reflect.DeepEqual(jsonRound(got), jsonRound(want)) {
			return fmt.Errorf("walk %d %v: state\n got %v\nwant %v", n, names, jsonRound(got), jsonRound(want))
		}
	}
	return nil
}

// terPaths decodes pathsJSONCover's walks.
func terPaths(cover tracecheck.Cover) ([]genPath, error) {
	b, err := pathsJSONCover("transcript_edit_retry", cover)
	if err != nil {
		return nil, err
	}
	var f struct {
		Paths []genPath `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return nil, err
	}
	if len(f.Paths) == 0 {
		return nil, errors.New("no walks over testdata/transcript_edit_retry")
	}
	return f.Paths, nil
}

// terHistory reads the abstract trace off a transcript. The composer's
// own steps leave nothing there, so the projection takes the one path
// every transcript also is: the first two turns were typed (or pasted)
// and sent, a third can only be an error card's Retry, an error entry
// and a done are Finish. inputs is "typed" when the recorded typed words
// carry no expansion: no [file:] or [skill:] block, no view_image
// sentence.
func terHistory(entries []history.Entry) []tracecheck.Step {
	ks, ends, inputs := []string{}, []string{}, []string{}
	state := func(status string) map[string]any {
		return map[string]any{
			"Session#0.status": status,
			"Session#0.kinds":  slices.Clone(ks),
			"Session#0.ends":   slices.Clone(ends),
			"Session#0.inputs": slices.Clone(inputs),
		}
	}
	step := func(action string, st map[string]any) tracecheck.Step {
		return tracecheck.Step{Action: "Session#0." + action, State: st}
	}
	steps := []tracecheck.Step{{Action: "Init", State: state("idle")}}
	failed := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			typed, _ := e.Data["typed"].(string)
			if typed == "" {
				typed, _ = e.Data["text"].(string)
			}
			k := terKind(typed)
			in := "typed"
			if strings.Contains(typed, "[file: ") || strings.Contains(typed, "[skill: ") || strings.Contains(typed, "call view_image") {
				in = "expanded"
			}
			n := len(ks)
			ks, ends, inputs = append(ks, k), append(ends, "running"), append(inputs, in)
			failed = false
			switch {
			case n >= 2:
				steps = append(steps, step("RetryTurn", state("running")))
			case k == "image":
				steps = append(steps, step("PasteImage", nil), step("UploadDone", nil), step("Send", state("running")))
			default:
				steps = append(steps, step("Type", nil), step("Send", state("running")))
			}
		case "error":
			failed = true
		case "done":
			if len(ends) == 0 || ends[len(ends)-1] != "running" {
				continue
			}
			end, status := "ok", "idle"
			if failed {
				end, status = "error", "error"
			}
			ends[len(ends)-1] = end
			steps = append(steps, step("Finish", state(status)))
		}
	}
	return steps
}

func init() { historyProjections["transcript_edit_retry"] = terHistory }

func TestTranscriptEditRetry(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTerAdapter(t)
	if err := runMBT(t, "transcript_edit_retry", a, terActions(a), terOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("walk steps: %v", a.stats)
	g, err := tracecheck.Load(fizzCheck(t, "transcript_edit_retry"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), terHistory)
	}
}

// A card Retry that resends the model's expansion instead of the typed
// words must fail the random run too.
func TestTranscriptEditRetryCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTerAdapter(t)
	a.retryExpanded = true
	if err := runMBT(t, "transcript_edit_retry", a, terActions(a), terOptions()); err == nil {
		t.Fatal("a run whose card Retry resends the expansion passed; the runner is not checking state")
	}
}

// The random runs rarely get past two steps; this walks the graph's
// walks (every settled state, or every link under
// MODEL_COVER=transitions) step by step against the adapter, comparing
// the whole role state after each, and replays every session's history
// on the graph.
func TestTranscriptEditRetryPaths(t *testing.T) {
	t.Parallel()
	paths, err := terPaths(envCover())
	if err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "transcript_edit_retry"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 4
	for sh := range shards {
		t.Run(fmt.Sprintf("shard%d", sh), func(t *testing.T) {
			t.Parallel()
			a := newTerAdapter(t)
			acts := terActions(a)
			for i := sh; i < len(paths); i += shards {
				if err := walkTerPath(a, acts, i, paths[i]); err != nil {
					t.Error(err)
				}
			}
			t.Logf("shard %d of %d walks: %v", sh, len(paths), a.stats)
			for _, id := range a.ids {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), terHistory)
			}
		})
	}
}

// The walks must fail on the wiring bug the random run is tested with,
// and there: the server records a card Retry of the expansion with the
// expansion as its typed words, so inputs reads "expanded" right after
// the first RetryTurn.
func TestTranscriptEditRetryPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	paths, err := terPaths(tracecheck.CoverStates)
	if err != nil {
		t.Fatal(err)
	}
	a := newTerAdapter(t)
	a.retryExpanded = true
	acts := terActions(a)
	for i, p := range paths {
		if err := walkTerPath(a, acts, i, p); err != nil {
			if msg := err.Error(); !strings.Contains(msg, " RetryTurn]: state") || !strings.Contains(msg, "expanded") {
				t.Fatalf("failed, but not on the card Retry's record: %v", err)
			}
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("walks whose card Retry resends the expansion passed")
}

// The history check must reject a transcript that recorded an expansion
// as the words typed: a resend of the expanded text records it so.
func TestTranscriptEditRetryHistoryRejectsExpanded(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "transcript_edit_retry"))
	if err != nil {
		t.Fatal(err)
	}
	typed := "read @" + terFile
	sent := typed + "\n\n[file: " + terFile + "]\nplain words"
	entries := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": sent, "typed": typed}},
		{Kind: "error", Data: map[string]any{"text": "model says no"}},
		{Kind: "done", Data: map[string]any{}},
	}
	if v := g.Check(terHistory(entries)); v != nil {
		t.Fatalf("a well-formed transcript is rejected: %v", v)
	}
	entries = append(entries, history.Entry{Kind: "input", Data: map[string]any{"text": sent + "\n\n[file: " + terFile + "]\nplain words", "typed": sent}})
	if g.Check(terHistory(entries)) == nil {
		t.Fatal("a transcript whose resend recorded the expansion as typed passed the history check")
	}
}
