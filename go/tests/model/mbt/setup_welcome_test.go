//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync/atomic"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/setup_welcome.fizz, the first-run welcome, driven at the server:
// the adapter plays welcome.tsx against a real serve. What the page
// keeps to itself (the welcome's mode, bough:welcome-done, the phone's
// pane, which path is in the field, a request in flight) the adapter
// tracks; everything the server knows is read back from it: whether a
// key is set (/api/setup), what the key check answered
// (/api/setup/check), what the folder check answered (/api/setup?cwd=)
// and whether a session exists (/api/sessions).
//
// The provider is a local stub the serve is pointed at with
// BOUGH_SETUP_CHECK_URL, answering as the next Key* action says; a proxy
// that goes nowhere catches any check that would still leave the machine.

// swStartDir is serve's working directory under HOME: the folder the
// welcome's field starts on ("plain": it exists and is no checkout).
const swStartDir = "start"

// swPrompt is the first chip, what StartOk sends.
const swPrompt = "Explain this repo"

type setupWelcomeAdapter struct {
	t    *testing.T
	gate gate

	// provider answers every key check with answer's status.
	provider *httptest.Server
	answer   atomic.Int32

	// A fresh serve per walk: a saved key lives in serve's process env,
	// and nothing in the API unsets it, so keyset cannot go back to false.
	s *servetest.Server

	// The page's own state, named as the spec names it.
	welcome, pane, key, typed, folder, start string
	done, skipped                            bool
	path                                     string // the field's text
	answered                                 string // the folder the last check answered for
	sending                                  string // the folder Start captured: edits while it is in flight do not change it
	beforeSave                               string // the key line a failed save leaves
	saves                                    int

	// Each started session, with the HOME its history is in.
	started []startedSession

	// rejectAsOK is the deliberate bug TestSetupWelcomeCatchesWrongAdapter
	// injects: the stub accepts the key KeyRejected should have refused.
	rejectAsOK bool
}

type startedSession struct{ home, id string }

func newSetupWelcomeAdapter(t *testing.T) *setupWelcomeAdapter {
	a := &setupWelcomeAdapter{t: t}
	a.provider = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(int(a.answer.Load()))
	}))
	t.Cleanup(a.provider.Close)
	return a
}

func (a *setupWelcomeAdapter) Init() error {
	if a.s != nil {
		a.s.Close()
	}
	a.s = servetest.Start(a.t, servetest.Options{
		Dir: swStartDir,
		Env: []string{
			"BOUGH_SETUP_CHECK_URL=" + a.provider.URL,
			// Loopback is never proxied, so only a check that slipped
			// past the stub goes here, and fails instead of going out.
			"HTTPS_PROXY=http://127.0.0.1:9", "HTTP_PROXY=http://127.0.0.1:9",
		},
	})
	for _, d := range []string{"plain", filepath.Join("repo", ".git")} {
		if err := os.MkdirAll(filepath.Join(a.s.Home, d), 0o755); err != nil {
			return err
		}
	}
	a.welcome, a.pane, a.start = "auto", "thread", "idle"
	a.done, a.skipped = false, false
	a.key, a.typed, a.folder = "unset", "plain", "checking"
	a.path, a.answered = "~/"+swStartDir, ""
	a.gate.reset()
	return nil
}

func (a *setupWelcomeAdapter) Cleanup() error { return nil }

func (a *setupWelcomeAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Setup", Index: 0}: a}, nil
}

// GetState reads keyset and session off the server every time; the rest
// is the page's, as the adapter last left it.
func (a *setupWelcomeAdapter) GetState() (map[string]any, error) {
	keyset, err := a.keySet()
	if err != nil {
		return nil, err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"welcome": a.welcome, "done": a.done, "skipped": a.skipped, "session": len(rows) > 0,
		"pane": a.pane, "keyset": keyset, "key": a.key, "typed": a.typed, "folder": a.folder, "start": a.start,
	}, nil
}

// The spec's helpers, over the adapter's view.
func (a *setupWelcomeAdapter) shown() bool {
	return !a.hasSession() && (a.welcome == "on" || a.welcome == "auto")
}

func (a *setupWelcomeAdapter) ready() bool {
	return a.key == "works" && (a.folder == "plain" || a.folder == "checkout") && a.start != "starting"
}

func (a *setupWelcomeAdapter) hasSession() bool {
	return len(a.started) > 0 && a.started[len(a.started)-1].home == a.s.Home
}

// --- step 1: the key ---------------------------------------------------

func (a *setupWelcomeAdapter) SaveKey() error {
	if !a.gate.pass(a.shown() && (a.key == "unset" || a.key == "rejected")) {
		return nil
	}
	a.beforeSave, a.key = a.key, "saving"
	return nil
}

func (a *setupWelcomeAdapter) SaveOk() error {
	if !a.gate.pass(a.key == "saving") {
		return nil
	}
	a.saves++
	if err := a.saveKey(fmt.Sprintf("sk-ant-%04d", a.saves)); err != nil {
		return err
	}
	a.key = "checking"
	return nil
}

// SaveFail sends a key the server refuses to write (a pasted newline):
// the save fails, and whatever key was set before stays set.
func (a *setupWelcomeAdapter) SaveFail() error {
	if !a.gate.pass(a.key == "saving") {
		return nil
	}
	var apiErr *servetest.APIError
	if err := a.saveKey("sk-ant\nbroken"); !errors.As(err, &apiErr) {
		return fmt.Errorf("a key with a newline in it: want the server to refuse it, got %v", err)
	}
	a.key = a.beforeSave
	return nil
}

func (a *setupWelcomeAdapter) KeyOk() error { return a.checkKey(http.StatusOK) }

func (a *setupWelcomeAdapter) KeyRejected() error {
	if a.rejectAsOK {
		return a.checkKey(http.StatusOK)
	}
	return a.checkKey(http.StatusUnauthorized)
}

// KeyUnknown is a provider that answers but not about the key: serve
// reports "unknown", which the page counts as working.
func (a *setupWelcomeAdapter) KeyUnknown() error { return a.checkKey(http.StatusBadGateway) }

func (a *setupWelcomeAdapter) checkKey(status int) error {
	if !a.gate.pass(a.key == "checking") {
		return nil
	}
	a.answer.Store(int32(status))
	var r struct {
		State string `json:"state"`
	}
	if err := a.api(http.MethodGet, "/api/setup/check?provider=anthropic", nil, &r); err != nil {
		return err
	}
	// keyLine: ok and unknown read the same, "key works".
	switch r.State {
	case "ok", "unknown":
		a.key = "works"
	default:
		a.key = r.State
	}
	return nil
}

// --- step 2: the folder ------------------------------------------------

func (a *setupWelcomeAdapter) EditMissing() error  { return a.edit("missing", "~/nowhere") }
func (a *setupWelcomeAdapter) EditPlain() error    { return a.edit("plain", "~/plain") }
func (a *setupWelcomeAdapter) EditCheckout() error { return a.edit("checkout", "~/repo") }

func (a *setupWelcomeAdapter) edit(typed, path string) error {
	if !a.gate.pass(a.shown() && a.typed != typed && a.key == "works" && a.folder != "checking") {
		return nil
	}
	a.typed, a.path, a.folder = typed, path, "checking"
	return nil
}

// FolderAnswer is the check for the path in the field, as welcome.tsx
// sends it (tilde and all), read the way its status line reads it.
func (a *setupWelcomeAdapter) FolderAnswer() error {
	if !a.gate.pass(a.folder == "checking") {
		return nil
	}
	var r struct {
		Folder struct {
			Path     string `json:"path"`
			Exists   bool   `json:"exists"`
			Checkout string `json:"checkout"`
		} `json:"folder"`
	}
	if err := a.api(http.MethodGet, "/api/setup?cwd="+url.QueryEscape(a.path), nil, &r); err != nil {
		return err
	}
	switch f := r.Folder; {
	case !f.Exists:
		a.folder = "missing"
	case f.Checkout != "":
		a.folder = "checkout"
	default:
		a.folder = "plain"
	}
	a.answered = r.Folder.Path
	return nil
}

// --- step 3: start -----------------------------------------------------

func (a *setupWelcomeAdapter) Start() error {
	if a.gate.pass(a.shown() && a.ready()) {
		a.start, a.sending = "starting", a.answered // go() reads folder.path once
	}
	return nil
}

// StartOk is the chip's POST /api/sessions for the folder the check
// answered; the new session opens, which unmounts the welcome, and only
// now is bough:welcome-done written.
func (a *setupWelcomeAdapter) StartOk() error {
	if !a.gate.pass(a.start == "starting") {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.sending, swPrompt)
	if err != nil {
		return err
	}
	a.started = append(a.started, startedSession{a.s.Home, row.ID})
	a.done = true
	a.key, a.typed, a.folder, a.start = "", "", "", "idle"
	return nil
}

// StartFail is the same POST after the folder went away between its
// check and the start: the server refuses it, and no session exists.
// The folder comes back at once, so the line the page still shows is
// true again.
func (a *setupWelcomeAdapter) StartFail() error {
	if !a.gate.pass(a.start == "starting") {
		return nil
	}
	aside := a.sending + ".aside"
	if err := os.Rename(a.sending, aside); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.CreateSession(ctx, a.sending, swPrompt)
	if rerr := os.Rename(aside, a.sending); rerr != nil {
		return rerr
	}
	var apiErr *servetest.APIError
	if !errors.As(err, &apiErr) {
		return fmt.Errorf("a start in a folder that is gone: want the server to refuse it, got %v", err)
	}
	a.start = "failed"
	return nil
}

// --- leaving and coming back -------------------------------------------

func (a *setupWelcomeAdapter) Skip() error {
	if a.gate.pass(a.shown()) {
		a.done, a.skipped, a.welcome, a.pane = true, true, "off", "list"
		a.key, a.typed, a.folder, a.start = "", "", "", "idle"
	}
	return nil
}

// ShowWelcome mounts the welcome afresh: it asks /api/setup which keys
// are set, and a set key is checked again.
func (a *setupWelcomeAdapter) ShowWelcome() error {
	if !a.gate.pass(!a.shown() && !a.hasSession()) {
		return nil
	}
	keyset, err := a.keySet()
	if err != nil {
		return err
	}
	a.welcome, a.pane, a.start = "on", "thread", "idle"
	a.key = "unset"
	if keyset {
		a.key = "checking"
	}
	a.typed, a.path, a.folder = "plain", "~/"+swStartDir, "checking"
	return nil
}

// --- the setup API, which servetest does not wrap ----------------------

func (a *setupWelcomeAdapter) keySet() (bool, error) {
	var r struct {
		Providers []struct {
			Name string `json:"name"`
			Set  bool   `json:"set"`
		} `json:"providers"`
	}
	if err := a.api(http.MethodGet, "/api/setup", nil, &r); err != nil {
		return false, err
	}
	for _, p := range r.Providers {
		if p.Name == "anthropic" {
			return p.Set, nil
		}
	}
	return false, errors.New("/api/setup lists no anthropic provider")
}

func (a *setupWelcomeAdapter) saveKey(key string) error {
	return a.api(http.MethodPost, "/api/setup/key", map[string]string{"provider": "anthropic", "key": key}, nil)
}

// api is one authenticated request; a non-2xx is a *servetest.APIError.
func (a *setupWelcomeAdapter) api(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

// end is the runner's name for the self-link fizz gives a state with no
// action out of it: here only a started session, where the flow ends.
func (a *setupWelcomeAdapter) end() error {
	a.gate.pass(a.hasSession())
	return nil
}

var setupWelcomeActions = map[string]map[string]fmbt.ActionFunc{"": {"end": action((*setupWelcomeAdapter).end)}, "Setup": {
	"end":          action((*setupWelcomeAdapter).end),
	"SaveKey":      action((*setupWelcomeAdapter).SaveKey),
	"SaveOk":       action((*setupWelcomeAdapter).SaveOk),
	"SaveFail":     action((*setupWelcomeAdapter).SaveFail),
	"KeyOk":        action((*setupWelcomeAdapter).KeyOk),
	"KeyRejected":  action((*setupWelcomeAdapter).KeyRejected),
	"KeyUnknown":   action((*setupWelcomeAdapter).KeyUnknown),
	"EditMissing":  action((*setupWelcomeAdapter).EditMissing),
	"EditPlain":    action((*setupWelcomeAdapter).EditPlain),
	"EditCheckout": action((*setupWelcomeAdapter).EditCheckout),
	"FolderAnswer": action((*setupWelcomeAdapter).FolderAnswer),
	"Start":        action((*setupWelcomeAdapter).Start),
	"StartOk":      action((*setupWelcomeAdapter).StartOk),
	"StartFail":    action((*setupWelcomeAdapter).StartFail),
	"Skip":         action((*setupWelcomeAdapter).Skip),
	"ShowWelcome":  action((*setupWelcomeAdapter).ShowWelcome),
}}

// The shortest start is six actions (save, saved, checked, folder
// answered, start, started), so a walk needs room past that.
func setupWelcomeOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 10, "max-parallel-runs": 0}
}

// setupWelcomeHistory reads what a transcript can say about this flow:
// a session exists, so the welcome started it, the end of a path the
// history cannot see the rest of (keys and folders leave nothing in a
// transcript). A transcript without its first prompt is no welcome
// start, and is left as a bare Init that claims a session: the check
// rejects it.
func setupWelcomeHistory(entries []history.Entry) []tracecheck.Step {
	for _, e := range entries {
		if e.Kind != "input" {
			continue
		}
		return []tracecheck.Step{
			{Action: "Init", State: map[string]any{"Setup#0.session": false}},
			{Action: "Setup#0.SaveKey"},
			{Action: "Setup#0.SaveOk"},
			{Action: "Setup#0.KeyOk"},
			{Action: "Setup#0.FolderAnswer"},
			{Action: "Setup#0.Start", State: map[string]any{"Setup#0.start": "starting"}},
			{Action: "Setup#0.StartOk", State: map[string]any{"Setup#0.session": true, "Setup#0.done": true}},
		}
	}
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Setup#0.session": true}}}
}

func init() { historyProjections["setup_welcome"] = setupWelcomeHistory }

// TestSetupWelcome is the runner's random walks. It picks each action
// uniformly from all sixteen, so a walk rarely gets past its second
// step (a start is six deep): it checks the shallow states and the
// gate, and TestSetupWelcomePaths below covers the rest.
func TestSetupWelcome(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSetupWelcomeAdapter(t)
	if err := runMBT(t, "setup_welcome", a, setupWelcomeActions, setupWelcomeOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// swFixtures is testdata/setup_welcome: the spec's graph and the paths
// generated from it, which TestSpecFixtures keeps current.
func swFixtures() string {
	return filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "setup_welcome")
}

type swPath struct {
	Target int               `json:"target"`
	Trace  []tracecheck.Step `json:"trace"`
}

// swPaths are the generated paths that cover every state and transition.
func swPaths(t *testing.T) []swPath {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(swFixtures(), "paths.json"))
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []swPath `json:"paths"`
	}
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatal(err)
	}
	return f.Paths
}

// walk drives one generated path through a fresh serve and compares the
// adapter's state with the spec's at every node. Every action on a path
// is enabled, so a closed gate is the adapter misreading a require.
func (a *setupWelcomeAdapter) walk(p swPath) error {
	for i, step := range p.Trace {
		name := strings.TrimPrefix(step.Action, "Setup#0.")
		var err error
		if i == 0 {
			err = a.Init()
		} else if f, ok := setupWelcomeActions["Setup"][name]; ok {
			_, err = f(a, nil)
		} else {
			err = fmt.Errorf("no adapter action for %q", step.Action)
		}
		if err == nil && a.gate.off {
			err = errors.New("the adapter's gate closed on an action the spec enables")
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, step.Action, err)
		}
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Setup#0.")
			if !ok {
				continue
			}
			if got[field] != want {
				return fmt.Errorf("step %d (%s): state mismatch for field %s: expected %v, actual %v", i, step.Action, field, want, got[field])
			}
		}
	}
	return nil
}

// TestSetupWelcomePaths walks every generated path (the ones the browser
// spec walks) against a real serve, then replays each started session's
// transcript on the graph. It needs no fizz tools: the paths and the
// graph are the checked-in fixtures.
func TestSetupWelcomePaths(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(swFixtures())
	if err != nil {
		t.Fatal(err)
	}
	for i, p := range swPaths(t) {
		t.Run(fmt.Sprintf("path%03d_to_%d", i, p.Target), func(t *testing.T) {
			t.Parallel()
			a := newSetupWelcomeAdapter(t)
			if err := a.walk(p); err != nil {
				t.Fatal(err)
			}
			for _, s := range a.started {
				// The echo turn on the first prompt has to close before its
				// transcript is the whole record of the start.
				if _, err := waitRow(a.s, s.id, "the first turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
					t.Fatal(err)
				}
				checkHistory(t, g, sessionHistory(t, s.home, s.id), setupWelcomeHistory)
			}
		})
	}
}

// A walk that accepts the key KeyRejected should refuse must fail: else
// a green TestSetupWelcomePaths proves nothing about the key line.
func TestSetupWelcomeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	for _, p := range swPaths(t) {
		if !slices.ContainsFunc(p.Trace, func(s tracecheck.Step) bool { return s.Action == "Setup#0.KeyRejected" }) {
			continue
		}
		a := newSetupWelcomeAdapter(t)
		a.rejectAsOK = true
		err := a.walk(p)
		if err == nil {
			t.Fatal("a walk whose KeyRejected is accepted passed; the paths are not checking state")
		}
		t.Logf("caught, as it should be: %v", err)
		return
	}
	t.Fatal("no generated path rejects a key")
}
