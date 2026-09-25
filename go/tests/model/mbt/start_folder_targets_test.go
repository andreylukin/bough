//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/start_folder_targets.fizz against a real serve: where a new
// session starts. The adapter is the page, so what the page holds (the
// palette, the dialog's field, the ModePicker, whether a load read
// failed) is its own state; what the server says is read off it: home
// and whether serve's start dir is a checkout (GET /api/health, GET
// /api/setup), whether P is on disk, whether the list's rows at P say
// it is missing, whether Q is listed, and where every POST /api/sessions
// the page sends lands (its answer's cwd and mode, or its 400).
//
// The fixture is the spec's: serve starts in S = H/work, a git
// checkout that also holds a folder named past; P = H/past has one past
// local session (a history file written by hand, so no child runs for
// it); Q is a project on the fake container runtime. A failed health or
// setup read is the page's: serve cannot be made to refuse one, so
// HealthFails and SetupFails only record it.
const sftConfig = controlConfig + "- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type sftAdapter struct {
	t    *testing.T
	s    *servetest.Server
	home string // H
	s0   string // S, serve's start dir
	past string // P
	gate gate

	slug string // Q
	qn   int

	// The page's state, as the spec names it.
	homeSt, start, mode, ui, typed, open string

	opened string   // the open session's id, "" on the Overview
	ids    []string // every session a walk created, for the trace check

	// tildeRaw is the deliberate bug TestStartFolderTargetsCatchesWrongAdapter
	// injects: the dialog sends "~/past" without expanding ~.
	tildeRaw bool
}

func newSFTAdapter(t *testing.T) *sftAdapter {
	s := servetest.Start(t, servetest.Options{Config: sftConfig, Dir: "work"})
	a := &sftAdapter{t: t, s: s, home: s.Home, s0: filepath.Join(s.Home, "work"), past: filepath.Join(s.Home, "past")}
	// S is a checkout (setup reports it; the checkout needs no commit),
	// with its own past, where a relative "past" would land.
	git := exec.Command("git", "-c", "init.defaultBranch=main", "init", "-q", a.s0)
	if out, err := git.CombinedOutput(); err != nil {
		t.Fatalf("git init: %v\n%s", err, out)
	}
	for _, d := range []string{filepath.Join(a.s0, "past"), a.past} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// The past session in P: a transcript is all the list needs.
	st, err := history.Open(filepath.Join(s.Home, ".bough", "history", history.NewID()+".jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	st.Append("meta", map[string]any{"cwd": a.past, "mode": "local", "origin": "web"})
	st.Append("input", map[string]any{"text": "earlier work in past"})
	if err := st.Close(); err != nil {
		t.Fatal(err)
	}
	return a
}

// Init puts the world back (P on disk, Q listed) and the page at load.
func (a *sftAdapter) Init() error {
	if err := os.MkdirAll(a.past, 0o755); err != nil {
		return err
	}
	listed, err := a.projectListed()
	if err != nil {
		return err
	}
	if !listed {
		a.qn++
		var r struct {
			Project struct {
				Slug string `json:"slug"`
			} `json:"project"`
		}
		if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Q %d", a.qn)}, &r); err != nil {
			return err
		}
		a.slug = r.Project.Slug
	}
	a.homeSt, a.start, a.mode, a.ui, a.typed, a.open = "loading", "loading", "local", "list", "none", "none"
	a.opened = ""
	a.gate.reset()
	return nil
}

// Cleanup archives the session a walk left open, so a walk's children
// do not pile up across hundreds of walks.
func (a *sftAdapter) Cleanup() error { return a.closeOpened() }

func (a *sftAdapter) closeOpened() error {
	if a.opened == "" {
		return nil
	}
	id := a.opened
	a.opened = ""
	// GET also lists a child that is still booting; archive becomes
	// available once its history file exists.
	for deadline := time.Now().Add(actionTimeout); ; {
		if _, err := os.Stat(filepath.Join(a.home, ".bough", "history", id+".jsonl")); err == nil {
			break
		} else if !errors.Is(err, os.ErrNotExist) {
			return err
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("session %s never wrote history", id)
		}
		time.Sleep(20 * time.Millisecond)
	}
	return a.api(http.MethodPost, "/api/sessions/"+id+"/archive", nil, nil)
}

func (a *sftAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

func (a *sftAdapter) GetState() (map[string]any, error) {
	folder := "gone"
	if st, err := os.Stat(a.past); err == nil && st.IsDir() {
		folder = "live"
	}
	prow, err := a.prow()
	if err != nil {
		return nil, err
	}
	listed, err := a.projectListed()
	if err != nil {
		return nil, err
	}
	project := "deleted"
	if listed {
		project = "listed"
	}
	return map[string]any{
		"home": a.homeSt, "start": a.start, "folder": folder, "prow": prow,
		"project": project, "mode": a.mode, "ui": a.ui, "typed": a.typed, "open": a.open,
	}, nil
}

// prow is how the list's rows at P read: every local row there must say
// the folder is missing once it is, and none before.
func (a *sftAdapter) prow() (string, error) {
	var r struct {
		Sessions []struct {
			Cwd        string `json:"cwd"`
			Mode       string `json:"mode"`
			CwdMissing bool   `json:"cwdMissing"`
		} `json:"sessions"`
	}
	if err := a.api(http.MethodGet, "/api/sessions", nil, &r); err != nil {
		return "", err
	}
	n, missing := 0, 0
	for _, row := range r.Sessions {
		if row.Cwd != a.past || row.Mode == "project" {
			continue
		}
		n++
		if row.CwdMissing {
			missing++
		}
	}
	switch {
	case n == 0:
		return "", errors.New("the list has no row at P: New would not offer it")
	case missing == 0:
		return "ok", nil
	case missing == n:
		return "missing", nil
	}
	return fmt.Sprintf("mixed(%d of %d missing)", missing, n), nil
}

func (a *sftAdapter) projectListed() (bool, error) {
	if a.slug == "" {
		return false, nil
	}
	var r struct {
		Projects []struct {
			Slug string `json:"slug"`
		} `json:"projects"`
	}
	if err := a.api(http.MethodGet, "/api/projects", nil, &r); err != nil {
		return false, err
	}
	for _, p := range r.Projects {
		if p.Slug == a.slug {
			return true, nil
		}
	}
	return false, nil
}

// api is one JSON call as the page makes it; a non-2xx is an
// *servetest.APIError so a caller can tell a refusal from a failure.
func (a *sftAdapter) api(method, path string, body, out any) error {
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
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return &servetest.APIError{Status: resp.StatusCode, Msg: fmt.Sprintf("%s %s: %s", method, path, bytes.TrimSpace(raw))}
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(raw, out)
}

// rowDir is the open session's folder as the palette's rowDir sees it.
func (a *sftAdapter) rowDir() string {
	if a.open == "orb" {
		return "home"
	}
	return a.open
}

// dirOf is the path the page holds for an abstract folder.
func (a *sftAdapter) dirOf(d string) string {
	switch d {
	case "start":
		return a.s0
	case "past":
		return a.past
	}
	return a.home
}

// newDir is where New starts with nothing (or a project session) open:
// S when setup said it is a checkout, else home.
func (a *sftAdapter) newDir() string {
	if a.start == "checkout" {
		return a.s0
	}
	return a.home
}

// create is the page's start(): POST /api/sessions, then the session
// opens (the palette closes) or the toast says it could not start.
func (a *sftAdapter) create(body map[string]any) error {
	var r struct {
		Session struct {
			ID   string `json:"id"`
			Cwd  string `json:"cwd"`
			Mode string `json:"mode"`
		} `json:"session"`
	}
	err := a.api(http.MethodPost, "/api/sessions", body, &r)
	a.typed = "none"
	if ae := (*servetest.APIError)(nil); errors.As(err, &ae) && ae.Status/100 == 4 {
		a.ui = "failed"
		return nil
	}
	if err != nil {
		return err
	}
	a.ids = append(a.ids, r.Session.ID)
	a.opened, a.ui = r.Session.ID, "list"
	switch {
	case r.Session.Mode == "project":
		a.open = "orb"
	case r.Session.Cwd == a.home:
		a.open = "home"
	case r.Session.Cwd == a.s0:
		a.open = "start"
	case r.Session.Cwd == a.past:
		a.open = "past"
	default:
		// Wherever serve resolved it: its own cwd, for a relative path.
		a.open = "servecwd"
		a.t.Logf("session %s started in %q", r.Session.ID, r.Session.Cwd)
	}
	return nil
}

func (a *sftAdapter) local(cwd string) error {
	return a.create(map[string]any{"cwd": cwd, "prompt": ""})
}

// orb is New in Q: the page sends home as the cwd, as app.tsx does.
func (a *sftAdapter) orb() error {
	return a.create(map[string]any{"cwd": a.home, "prompt": "", "mode": "project", "project": a.slug})
}

// --- the two reads at load

func (a *sftAdapter) HealthOk() error {
	if !a.gate.pass(a.homeSt != "known") {
		return nil
	}
	var r struct {
		OK   bool   `json:"ok"`
		Home string `json:"home"`
	}
	if err := a.api(http.MethodGet, "/api/health", nil, &r); err != nil {
		return err
	}
	if !r.OK || r.Home != a.home {
		return fmt.Errorf("HealthOk: health said ok=%v home=%q, want home %q", r.OK, r.Home, a.home)
	}
	a.homeSt = "known"
	return nil
}

func (a *sftAdapter) HealthFails() error {
	if a.gate.pass(a.homeSt == "loading") {
		a.homeSt = "failed"
	}
	return nil
}

// SetupAnswers reads start from folder.checkout and home from setup's
// home: the page needs no health answer to know where New starts.
func (a *sftAdapter) SetupAnswers() error {
	if !a.gate.pass(a.start == "loading") {
		return nil
	}
	var r struct {
		Home   string `json:"home"`
		Folder struct {
			Path     string `json:"path"`
			Checkout string `json:"checkout"`
		} `json:"folder"`
	}
	if err := a.api(http.MethodGet, "/api/setup", nil, &r); err != nil {
		return err
	}
	a.start = "none"
	if r.Folder.Checkout != "" {
		if r.Folder.Path != a.s0 {
			return fmt.Errorf("SetupAnswers: setup's folder is %q, serve started in %q", r.Folder.Path, a.s0)
		}
		a.start = "checkout"
	}
	if r.Home == a.home {
		a.homeSt = "known"
	}
	return nil
}

func (a *sftAdapter) SetupFails() error {
	if a.gate.pass(a.start == "loading") {
		a.start = "none"
	}
	return nil
}

// --- the palette

func (a *sftAdapter) OpenNewPalette() error {
	if a.gate.pass(a.ui == "list") {
		a.ui = "palette"
	}
	return nil
}

func (a *sftAdapter) Escape() error {
	if a.gate.pass(a.ui == "palette" || a.ui == "dialog") {
		a.ui, a.typed = "list", "none"
	}
	return nil
}

func (a *sftAdapter) paletteReady() bool { return a.ui == "palette" && a.homeSt == "known" }

// PickSuggested is Enter on the suggested Start row: new:cwd (the open
// session's folder), else new:start (S) or new:here (home).
func (a *sftAdapter) PickSuggested() error {
	if !a.gate.pass(a.paletteReady()) {
		return nil
	}
	if err := a.closeOpened(); err != nil {
		return err
	}
	if d := a.rowDir(); d != "none" && d != "home" {
		return a.local(a.dirOf(d))
	}
	return a.local(a.newDir())
}

// PickPast is new:dir:P, which the page lists only while a row at P is
// in the list.
func (a *sftAdapter) PickPast() error {
	if !a.gate.pass(a.paletteReady() && a.rowDir() != "past") {
		return nil
	}
	if _, err := a.prow(); err != nil {
		return fmt.Errorf("PickPast: %w", err)
	}
	if err := a.closeOpened(); err != nil {
		return err
	}
	return a.local(a.past)
}

func (a *sftAdapter) PickOrb() error {
	if !a.gate.pass(a.paletteReady() && a.slug != "" && a.mustListed()) {
		return nil
	}
	if err := a.closeOpened(); err != nil {
		return err
	}
	return a.orb()
}

// mustListed is the require on Q, read off the server.
func (a *sftAdapter) mustListed() bool {
	ok, err := a.projectListed()
	return err == nil && ok
}

func (a *sftAdapter) AskFolder() error {
	if a.gate.pass(a.paletteReady()) {
		a.ui, a.typed = "dialog", "none"
	}
	return nil
}

// --- the "Start in folder" dialog

func (a *sftAdapter) typeInto(what string) error {
	if a.gate.pass(a.ui == "dialog" && a.typed == "none") {
		a.typed = what
	}
	return nil
}

func (a *sftAdapter) TypeTilde() error    { return a.typeInto("tilde") }
func (a *sftAdapter) TypeRelative() error { return a.typeInto("relative") }
func (a *sftAdapter) TypeMissing() error  { return a.typeInto("missing") }

// SubmitPath is the dialog's Start: "~" expands to home, anything else
// is sent as typed.
func (a *sftAdapter) SubmitPath() error {
	if !a.gate.pass(a.ui == "dialog" && a.typed != "none") {
		return nil
	}
	typed := map[string]string{"tilde": "~/past", "relative": "past", "missing": "~/nope"}[a.typed]
	p := typed
	if rest, ok := strings.CutPrefix(typed, "~"); ok && !a.tildeRaw {
		p = a.home + rest
	}
	if err := a.closeOpened(); err != nil {
		return err
	}
	return a.local(p)
}

// --- the world changes under the page

func (a *sftAdapter) DeleteFolderOnDisk() error {
	if !a.gate.pass(a.ui == "list" && dirExists(a.past)) {
		return nil
	}
	// P is under this serve's temp root; a session started there may
	// have left files in it.
	if !strings.HasPrefix(a.past, a.s.Root+string(filepath.Separator)) {
		return fmt.Errorf("DeleteFolderOnDisk: %q is not under the test root", a.past)
	}
	return os.RemoveAll(a.past)
}

func dirExists(p string) bool {
	st, err := os.Stat(p)
	return err == nil && st.IsDir()
}

// ProjectDeletedElsewhere is another tab's Delete; the page's next
// projects read drops Q and the ModePicker falls back to Local.
func (a *sftAdapter) ProjectDeletedElsewhere() error {
	if !a.gate.pass(a.ui == "list" && a.mustListed()) {
		return nil
	}
	if err := a.api(http.MethodDelete, "/api/projects/"+a.slug, nil, nil); err != nil {
		return err
	}
	a.mode = "local"
	// Deleting Q ends its threads, and one still booting never writes
	// history: there is nothing left to archive or trace-check.
	if a.open == "orb" && a.opened != "" {
		if _, err := os.Stat(filepath.Join(a.s.Home, ".bough", "history", a.opened+".jsonl")); err != nil {
			a.ids = slices.DeleteFunc(a.ids, func(id string) bool { return id == a.opened })
			a.opened = ""
		}
	}
	return nil
}

// --- the Overview

func (a *sftAdapter) overview() bool {
	return a.ui == "list" && a.open == "none" && a.homeSt == "known"
}

func (a *sftAdapter) PickProjectMode() error {
	if a.gate.pass(a.overview() && a.mode == "local" && a.mustListed()) {
		a.mode = "project"
	}
	return nil
}

func (a *sftAdapter) PickLocalMode() error {
	if a.gate.pass(a.overview() && a.mode == "project") {
		a.mode = "local"
	}
	return nil
}

func (a *sftAdapter) OverviewNew() error {
	if !a.gate.pass(a.overview()) {
		return nil
	}
	if a.mode == "project" {
		return a.orb()
	}
	return a.local(a.newDir())
}

func (a *sftAdapter) Close() error {
	if !a.gate.pass(a.ui == "list" && a.open != "none") {
		return nil
	}
	a.open = "none"
	return a.closeOpened()
}

func (a *sftAdapter) Dismiss() error {
	if a.gate.pass(a.ui == "failed") {
		a.ui = "list"
	}
	return nil
}

var sftActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"HealthOk":                action((*sftAdapter).HealthOk),
	"HealthFails":             action((*sftAdapter).HealthFails),
	"SetupAnswers":            action((*sftAdapter).SetupAnswers),
	"SetupFails":              action((*sftAdapter).SetupFails),
	"OpenNewPalette":          action((*sftAdapter).OpenNewPalette),
	"Escape":                  action((*sftAdapter).Escape),
	"PickSuggested":           action((*sftAdapter).PickSuggested),
	"PickPast":                action((*sftAdapter).PickPast),
	"PickOrb":                 action((*sftAdapter).PickOrb),
	"AskFolder":               action((*sftAdapter).AskFolder),
	"TypeTilde":               action((*sftAdapter).TypeTilde),
	"TypeRelative":            action((*sftAdapter).TypeRelative),
	"TypeMissing":             action((*sftAdapter).TypeMissing),
	"SubmitPath":              action((*sftAdapter).SubmitPath),
	"DeleteFolderOnDisk":      action((*sftAdapter).DeleteFolderOnDisk),
	"ProjectDeletedElsewhere": action((*sftAdapter).ProjectDeletedElsewhere),
	"PickProjectMode":         action((*sftAdapter).PickProjectMode),
	"PickLocalMode":           action((*sftAdapter).PickLocalMode),
	"OverviewNew":             action((*sftAdapter).OverviewNew),
	"Close":                   action((*sftAdapter).Close),
	"Dismiss":                 action((*sftAdapter).Dismiss),
}}

// startFolderTargetsHistory reads a created session's transcript as the
// shortest page path that starts it. The transcript names only where
// the session runs, so the folder is told by its name, which the
// fixture fixes (S is "work", P is "past", home is "home"): a relative
// cwd, or one under S, is serve's cwd, a place the spec never allows.
func startFolderTargetsHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Page#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	open := "home"
	for _, e := range entries {
		if e.Kind != "meta" {
			continue
		}
		cwd, _ := e.Data["cwd"].(string)
		mode, _ := e.Data["mode"].(string)
		switch {
		case mode == "project":
			open = "orb"
		case !filepath.IsAbs(cwd) || filepath.Base(filepath.Dir(cwd)) == "work":
			open = "servecwd"
		case filepath.Base(cwd) == "past":
			open = "past"
		case filepath.Base(cwd) == "work":
			open = "start"
		}
		break
	}
	steps := []tracecheck.Step{{Action: "Init", State: q("ui", "list", "open", "none")}}
	switch open {
	case "home":
		return append(steps,
			tracecheck.Step{Action: "Page#0.SetupFails", State: q("start", "none")},
			tracecheck.Step{Action: "Page#0.HealthOk", State: q("home", "known")},
			tracecheck.Step{Action: "Page#0.OverviewNew", State: q("ui", "list", "open", "home")})
	case "start":
		return append(steps,
			tracecheck.Step{Action: "Page#0.SetupAnswers", State: q("start", "checkout", "home", "known")},
			tracecheck.Step{Action: "Page#0.OverviewNew", State: q("ui", "list", "open", "start")})
	case "orb":
		return append(steps,
			tracecheck.Step{Action: "Page#0.SetupAnswers", State: q("start", "checkout", "home", "known")},
			tracecheck.Step{Action: "Page#0.OpenNewPalette", State: q("ui", "palette")},
			tracecheck.Step{Action: "Page#0.PickOrb", State: q("ui", "list", "open", "orb")})
	case "past":
		return append(steps,
			tracecheck.Step{Action: "Page#0.SetupAnswers", State: q("start", "checkout", "home", "known")},
			tracecheck.Step{Action: "Page#0.OpenNewPalette", State: q("ui", "palette")},
			tracecheck.Step{Action: "Page#0.PickPast", State: q("ui", "list", "open", "past")})
	}
	// Only the dialog's typed path could have put it there.
	return append(steps,
		tracecheck.Step{Action: "Page#0.SetupAnswers", State: q("start", "checkout", "home", "known")},
		tracecheck.Step{Action: "Page#0.OpenNewPalette", State: q("ui", "palette")},
		tracecheck.Step{Action: "Page#0.AskFolder", State: q("ui", "dialog")},
		tracecheck.Step{Action: "Page#0.TypeRelative", State: q("typed", "relative")},
		tracecheck.Step{Action: "Page#0.SubmitPath", State: q("ui", "list", "open", "servecwd")})
}

func init() { historyProjections["start_folder_targets"] = startFolderTargetsHistory }

// walkStartFolderTargetsPaths walks every path pathsJSONCover derives
// for the cover and compares the adapter's state with the spec's after
// each step. failFast stops at the first divergence (the wrong-adapter
// test needs one, not all).
func walkStartFolderTargetsPaths(t *testing.T, a *sftAdapter, cover tracecheck.Cover, failFast bool) error {
	t.Helper()
	b, err := pathsJSONCover("start_folder_targets", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Page#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
			if failFast {
				break
			}
		}
	}
	return errors.Join(errs...)
}

func (a *sftAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, s := range trace {
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = sftActions["Page"][strings.TrimPrefix(s.Action, "Page#0.")](a, nil)
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, s.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, s.Action, err)
		}
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Page#0.")
			if ok && got[f] != v {
				return fmt.Errorf("step %d (%s): %s is %v, the spec says %v (state %v)", j, s.Action, f, got[f], v, got)
			}
		}
	}
	return nil
}

// A walk of 10 reaches a pick and the step after it.
func startFolderTargetsOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 10, "max-parallel-runs": 0}
}

// TestStartFolderTargets lets fizzbee-mbt walk the spec at random: 21
// actions with a few enabled at a time, so it rarely gets deep;
// TestStartFolderTargetsPaths is the cover.
func TestStartFolderTargets(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSFTAdapter(t)
	if err := runMBT(t, "start_folder_targets", a, sftActions, startFolderTargetsOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkStartFolderTargetsHistories(t, a)
}

// TestStartFolderTargetsPaths walks every derived path against one serve.
func TestStartFolderTargetsPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSFTAdapter(t)
	if err := walkStartFolderTargetsPaths(t, a, envCover(), false); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	if len(a.ids) == 0 {
		t.Fatal("no path created a session")
	}
	checkStartFolderTargetsHistories(t, a)
}

// Every session a walk created left a transcript that is a path too.
func checkStartFolderTargetsHistories(t *testing.T, a *sftAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "start_folder_targets"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), startFolderTargetsHistory)
	}
	t.Logf("trace-checked %d created sessions' histories", len(a.ids))
}

// The projection is only a check if a transcript the model forbids is
// refused: a session started in serve's own cwd.
func TestStartFolderTargetsHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("start_folder_targets")), "..", "testdata", "start_folder_targets"))
	if err != nil {
		t.Fatal(err)
	}
	meta := func(cwd, mode string) []history.Entry {
		return []history.Entry{{Kind: "meta", Data: map[string]any{"cwd": cwd, "mode": mode}}}
	}
	for name, es := range map[string][]history.Entry{
		"home":  meta("/t/home", "local"),
		"start": meta("/t/home/work", "local"),
		"past":  meta("/t/home/past", "local"),
		"orb":   meta("/t/home/.bough/orbs/x/wt", "project"),
	} {
		if v := g.Check(startFolderTargetsHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	for name, es := range map[string][]history.Entry{
		"relative":      meta("past", "local"),
		"resolved at S": meta("/t/home/work/past", "local"),
	} {
		if v := g.Check(startFolderTargetsHistory(es)); v == nil {
			t.Errorf("%s: a session in serve's cwd passed the trace check", name)
		} else {
			t.Logf("%s refused as expected: %v", name, v)
		}
	}
}

// A dialog that sends "~/past" unexpanded must fail the walks, or a
// green TestStartFolderTargetsPaths proves nothing. It shows only on
// SubmitPath after TypeTilde, so every link is walked, stopping at the
// first divergence.
func TestStartFolderTargetsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSFTAdapter(t)
	a.tildeRaw = true
	err := walkStartFolderTargetsPaths(t, a, tracecheck.CoverTransitions, true)
	if err == nil {
		t.Fatal("a run whose dialog sends ~ unexpanded passed; the paths are not checking state")
	}
	// Caught for that reason, not for something else going wrong.
	// (The first field compared is map order: ui or open.)
	if msg := err.Error(); !strings.Contains(msg, "(Page#0.SubmitPath): ui is failed, the spec says list") &&
		!strings.Contains(msg, "(Page#0.SubmitPath): open is none, the spec says past") {
		t.Fatalf("the run failed, but not on the unexpanded ~: %v", err)
	}
	t.Logf("caught as expected: %v", err)
}
