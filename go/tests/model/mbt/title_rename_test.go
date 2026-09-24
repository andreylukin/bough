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
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/title_rename.fizz against a real serve: where a session's name
// comes from (history's title, serve's meta rename) and what a page that
// read the list shows.
//
// The spec's values are abstract; on the wire they are these texts. The
// prompt is literally "prompt" so history's opening-line title needs no
// mapping, and llm-control answers every small-model call with
// "control", which the session-title plugin writes as the final name.
const (
	titlePromptText = "prompt"
	titleAutoText   = "control"
	titleMineText   = "mine"
)

// titleAbstract maps a name the server shows back to the spec's value.
func titleAbstract(s string) string {
	if s == titleAutoText {
		return "auto"
	}
	return s
}

// titleRenameAdapter plays the page: sidebar, header and tab are what it
// last read off GET /api/sessions (a poll, or the refresh a rename
// awaits), and the dialog is its own. hist, meta and title are read off
// the server on every GetState: history from the transcript, meta from
// serve's meta.json on disk (what survives a restart), title from the
// list row.
type titleRenameAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate

	id                   string
	sidebar, header, tab string
	dialog, failed       bool
	prior                string
	turn                 int
	held                 string // the first turn, held until AutoTitle
	ids                  []string

	// renameNoop is the deliberate bug TestTitleRenameCatchesWrongAdapter
	// injects: Rename closes the dialog without posting.
	renameNoop bool
}

func newTitleRenameAdapter(t *testing.T) *titleRenameAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &titleRenameAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *titleRenameAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	a.sidebar, a.header, a.tab = "", "", ""
	a.dialog, a.failed, a.prior = false, false, ""
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup lets the held first turn finish so its request is not left
// hanging while the next walk queues its own.
func (a *titleRenameAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *titleRenameAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

func (a *titleRenameAdapter) GetState() (map[string]any, error) {
	hist, err := a.hist()
	if err != nil {
		return nil, err
	}
	meta, err := a.meta()
	if err != nil {
		return nil, err
	}
	title, err := a.title()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"hist": titleAbstract(hist), "meta": titleAbstract(meta), "title": titleAbstract(title),
		"sidebar": titleAbstract(a.sidebar), "header": titleAbstract(a.header), "tab": titleAbstract(a.tab),
		"dialog": a.dialog, "failed": a.failed, "prior": titleAbstract(a.prior),
	}, nil
}

// hist is history's title as the product derives it (history.List).
func (a *titleRenameAdapter) hist() (string, error) {
	infos, err := history.List(filepath.Join(a.s.Home, ".bough", "history"))
	if err != nil {
		return "", err
	}
	for _, in := range infos {
		if in.ID == a.id {
			return in.Title, nil
		}
	}
	return "", nil // no transcript yet: nothing names it
}

// metaPath is where serve keeps meta (cmd/bough/serve.go).
func (a *titleRenameAdapter) metaPath() string {
	return filepath.Join(a.s.Home, ".bough", "serve", "meta.json")
}

// meta is the rename serve has saved, read off disk: a title only held
// in memory is gone after a restart, so it is not the session's meta.
func (a *titleRenameAdapter) meta() (string, error) {
	b, err := os.ReadFile(a.metaPath())
	if errors.Is(err, os.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	var f struct {
		Sessions map[string]serve.SessionMeta `json:"sessions"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return "", fmt.Errorf("parse %s: %w", a.metaPath(), err)
	}
	return f.Sessions[a.id].Title, nil
}

// title is the session's row as the list (what the page polls) has it.
func (a *titleRenameAdapter) title() (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, true)
	if err != nil {
		return "", err
	}
	for _, r := range rows {
		if r.ID == a.id {
			return r.Title, nil
		}
	}
	return "", fmt.Errorf("session %s not in the list", a.id)
}

// waitHist waits for history's title to read want: the input and the
// title entry land in the transcript a moment after the API answers.
func (a *titleRenameAdapter) waitHist(want string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var got string
	for {
		h, err := a.hist()
		if err != nil {
			return err
		}
		if got = h; got == want {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for history's title %q: still %q", want, got)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// refresh is the page's list read: every surface takes the row's title.
func (a *titleRenameAdapter) refresh() error {
	title, err := a.title()
	if err != nil {
		return err
	}
	a.sidebar, a.header, a.tab = title, title, title
	return nil
}

// rename is the dialog's POST /api/sessions/{id}/rename. servetest has
// no verb for it and a flow edits nothing shared, so it is posted here.
func (a *titleRenameAdapter) rename(ctx context.Context, title string) error {
	body, _ := json.Marshal(map[string]string{"title": title})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/rename", bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	msg, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("rename %q: %d: %s", title, resp.StatusCode, bytes.TrimSpace(msg))
	}
	return nil
}

// Prompt sends the first prompt and holds its turn, so history's title
// is the opening line until AutoTitle lets the turn end.
func (a *titleRenameAdapter) Prompt() error {
	hist, err := a.hist()
	if err != nil {
		return err
	}
	if !a.gate.pass(hist == "") {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("r%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, titlePromptText); err != nil {
		return err
	}
	a.held = name
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	return a.waitHist(titlePromptText)
}

// AutoTitle ends the first turn; the session-title plugin then writes
// its one final name.
func (a *titleRenameAdapter) AutoTitle() error {
	hist, err := a.hist()
	if err != nil {
		return err
	}
	if !a.gate.pass(hist == titlePromptText && a.held != "") {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	return a.waitHist(titleAutoText)
}

func (a *titleRenameAdapter) Poll() error {
	title, err := a.title()
	if err != nil {
		return err
	}
	if !a.gate.pass(a.sidebar != title) {
		return nil
	}
	return a.refresh()
}

func (a *titleRenameAdapter) OpenRename() error {
	if !a.gate.pass(!a.dialog) {
		return nil
	}
	meta, err := a.meta()
	if err != nil {
		return err
	}
	a.dialog, a.failed, a.prior = true, false, meta
	return nil
}

// Rename posts the dialog's text and, as onRename does, awaits the list
// refresh before the dialog closes.
func (a *titleRenameAdapter) Rename(args []fmbt.Arg) error {
	if len(args) != 1 {
		return fmt.Errorf("Rename: want one choice argument, got %v", args)
	}
	text, ok := args[0].Value.(string)
	if !ok {
		return fmt.Errorf("Rename: choice %v is not a string", args[0].Value)
	}
	if !a.gate.pass(a.dialog && (text == "" || text != a.header)) {
		return nil
	}
	if !a.renameNoop {
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.rename(ctx, text); err != nil {
			return err
		}
		if err := a.refresh(); err != nil {
			return err
		}
	}
	a.dialog, a.failed, a.prior = false, false, ""
	return nil
}

// RenameFail makes serve's meta save fail (a directory where it writes
// meta.json.tmp), posts a rename that would change the name, and puts
// the save path back. The dialog shows the error and stays up.
func (a *titleRenameAdapter) RenameFail() error {
	if !a.gate.pass(a.dialog) {
		return nil
	}
	meta, err := a.meta()
	if err != nil {
		return err
	}
	text := titleMineText
	if meta == titleMineText {
		text = ""
	}
	tmp := a.metaPath() + ".tmp"
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	perr := a.rename(ctx, text)
	if err := os.Remove(tmp); err != nil {
		return err
	}
	if perr == nil {
		return fmt.Errorf("RenameFail: POST rename %q succeeded with meta.json.tmp blocked", text)
	}
	a.failed = true
	return nil
}

func (a *titleRenameAdapter) Dismiss() error {
	if a.gate.pass(a.dialog) {
		a.dialog, a.failed, a.prior = false, false, ""
	}
	return nil
}

var titleRenameActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":     action((*titleRenameAdapter).Prompt),
	"AutoTitle":  action((*titleRenameAdapter).AutoTitle),
	"Poll":       action((*titleRenameAdapter).Poll),
	"OpenRename": action((*titleRenameAdapter).OpenRename),
	"Rename": func(m any, args []fmbt.Arg) (any, error) {
		return nil, m.(*titleRenameAdapter).Rename(args)
	},
	"RenameFail": action((*titleRenameAdapter).RenameFail),
	"Dismiss":    action((*titleRenameAdapter).Dismiss),
}}

// Walks end at their first disabled pick, and most reach no Rename: 100
// walks ran about 3 Renames, too few for the wrong-adapter test to be
// sure to meet one; 300 run about a dozen in ~35 s.
func titleRenameOptions() map[string]any {
	return map[string]any{"max-seq-runs": 300, "max-actions": 8, "max-parallel-runs": 0}
}

// titleRenameHistory reads the trace off a transcript: history holds
// only what the session and the small model wrote, never a rename, so
// the steps are the first input (Prompt) and the first "title" entry
// (AutoTitle), checked on hist alone.
func titleRenameHistory(entries []history.Entry) []tracecheck.Step {
	hist := func(s string) map[string]any { return map[string]any{"Session#0.hist": s} }
	steps := []tracecheck.Step{{Action: "Init", State: hist("")}}
	prompted, named := false, false
	for _, e := range entries {
		switch {
		case e.Kind == "input" && !prompted:
			prompted = true
			steps = append(steps, tracecheck.Step{Action: "Session#0.Prompt", State: hist(titleAbstract(history.Prompt(e)))})
		case e.Kind == "title" && !named:
			named = true
			text, _ := e.Data["text"].(string)
			steps = append(steps, tracecheck.Step{Action: "Session#0.AutoTitle", State: hist(titleAbstract(text))})
		}
	}
	return steps
}

func init() { historyProjections["title_rename"] = titleRenameHistory }

func TestTitleRename(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTitleRenameAdapter(t)
	if err := runMBT(t, "title_rename", a, titleRenameActions, titleRenameOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "title_rename"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), titleRenameHistory)
	}
}

// A Rename that never reaches serve leaves meta and the row as they
// were while the spec renames; the run must say so.
func TestTitleRenameCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newTitleRenameAdapter(t)
	a.renameNoop = true
	if err := runMBT(t, "title_rename", a, titleRenameActions, titleRenameOptions()); err == nil {
		t.Fatal("a run whose Rename never posts passed; the runner is not checking state")
	}
}
