//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
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

// specs/project_definition_writers.fizz against a real serve: one
// project per walk with a MEMORY.md, the project page's editor (played
// by the adapter: it holds what the page last read and the draft, and
// saves and re-reads through the API the page calls), an agent and a
// guest shell writing the file straight into ~/.bough/projects/<slug>
// (they have no API: the file tools and the read-write mount put bytes
// on disk), DELETE from another tab, and a local session filed under
// the project whose every turn context-md reads MEMORY.md for.
//
// What a turn sent the model is read off the engine's parts file
// (~/.bough/engine/<sid>.parts.json): the pieces the model was last
// brought up to, the frozen prompt's and every <context-update>'s. The
// session is local and filed, not a project session, because a filed
// session outlives Delete (serve ends it and unfiles it; the next
// message resumes it with no project), which is how a turn after the
// delete finds MEMORY.md gone. A filed session reads MEMORY.md only
// from its next start, so Init files it idle and restarts it
// (archive, unarchive) before its first turn.
//
// No orbs: BOUGH_CONTAINER=none, as in project_lifecycle.

// The texts. What each mem value is on disk is told by which sections
// the file has, so a save and an agent's write compose the way the
// spec's values do whichever wrote first.
const (
	pdwV0     = "# Brief\n\nKeep the widget small.\n"
	pdwFact   = "\n# Remembered\n\nThe build needs Go 1.27.\n"
	pdwPerson = "\n# Person\n\nEdited on the project page (%d).\n"
	pdwHost   = "# Host notes\n\nNot this project's.\n"
)

// pdwClassify names a MEMORY.md text as the spec does.
func pdwClassify(text string) string {
	fact, person := strings.Contains(text, "# Remembered"), strings.Contains(text, "# Person")
	switch {
	case strings.Contains(text, "# Host notes"):
		return "host"
	case fact && person:
		return "both"
	case person:
		return "edit"
	case fact:
		return "agent"
	case strings.Contains(text, "# Brief"):
		return "v0"
	}
	return fmt.Sprintf("unknown %q", text)
}

type pdwAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	cwd  string // the sessions' working directory
	host string // the host file a guest links MEMORY.md to
	gate gate

	walk  int
	slug  string
	sid   string
	turns int
	held  string // a block turn not yet released

	// The page: its copy of MEMORY.md as last read, and the draft.
	fm, fmText string
	draftText  string
	edited     bool
	edErr      bool
	edits      int

	sessions []string // every walk's session, for the trace check

	// mergeOnSave is the deliberate wiring bug the PathsCatchWrongAdapter
	// test injects: the page re-reads MEMORY.md and keeps what an agent
	// added before it saves, which the product does not do.
	mergeOnSave bool
}

func newPDWAdapter(t *testing.T) *pdwAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"BOUGH_CONTAINER=none"}})
	a := &pdwAdapter{t: t, s: s, dir: control.Dir(s.Home), cwd: s.Dir(t, "work")}
	a.host = filepath.Join(s.Dir(t, "host"), "notes.md")
	if err := os.WriteFile(a.host, []byte(pdwHost), 0o644); err != nil {
		t.Fatal(err)
	}
	return a
}

func (a *pdwAdapter) projDir() string {
	return filepath.Join(a.s.Home, ".bough", "projects", a.slug)
}

func (a *pdwAdapter) memPath() string { return filepath.Join(a.projDir(), "MEMORY.md") }

// pdwView is the spec's state as serve and the disk have it, plus the
// adapter's page.
type pdwView struct {
	dir          bool
	mem, fm, inj string
	draft, edErr bool
}

func (a *pdwAdapter) view() (pdwView, error) {
	v := pdwView{fm: a.fm, draft: a.edited, edErr: a.edErr}
	if st, err := os.Stat(a.projDir()); err == nil && st.IsDir() {
		v.dir = true
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return v, err
	}
	switch st, err := os.Lstat(a.memPath()); {
	case errors.Is(err, os.ErrNotExist):
		v.mem = "none"
		if v.dir {
			v.mem = "missing" // not a spec value: a mismatch that names itself
		}
	case err != nil:
		return v, err
	case st.Mode()&os.ModeSymlink != 0:
		v.mem = "link"
	default:
		b, err := os.ReadFile(a.memPath())
		if err != nil {
			return v, err
		}
		v.mem = pdwClassify(string(b))
	}
	inj, err := a.injected()
	if err != nil {
		return v, err
	}
	v.inj = inj
	return v, nil
}

// injected is the MEMORY.md the model was last brought up to: the
// piece of the engine's parts file named by the project's MEMORY.md
// path, "none" when there is none.
func (a *pdwAdapter) injected() (string, error) {
	b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "engine", a.sid+".parts.json"))
	if err != nil {
		return "", fmt.Errorf("the engine's parts file: %w", err)
	}
	var parts struct {
		Context []struct{ Name, Text string }
	}
	if err := json.Unmarshal(b, &parts); err != nil {
		return "", err
	}
	for _, p := range parts.Context {
		if strings.HasSuffix(p.Name, filepath.Join("projects", a.slug, "MEMORY.md")) {
			return pdwClassify(p.Text), nil
		}
	}
	return "none", nil
}

func (a *pdwAdapter) state(v pdwView) map[string]any {
	return map[string]any{"dir": v.dir, "mem": v.mem, "fm": v.fm, "draft": v.draft, "ed_err": v.edErr, "inj": v.inj}
}

func (a *pdwAdapter) api(method, path string, body, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	return apiCall(ctx, a.s, method, path, body, out)
}

// readFiles is the page's loadFiles: GET .../orb, MEMORY.md's text.
func (a *pdwAdapter) readFiles() error {
	var d serve.OrbDetail
	if err := a.api(http.MethodGet, "/api/projects/"+a.slug+"/orb", nil, &d); err != nil {
		return err
	}
	a.fm = pdwClassify(d.Files["MEMORY.md"])
	a.fmText = d.Files["MEMORY.md"]
	return nil
}

// Init starts each walk on a fresh project with MEMORY.md v0, its page
// open, and a filed session whose one turn sent v0.
func (a *pdwAdapter) Init() error {
	a.walk++
	var r struct {
		Project serve.Project `json:"project"`
	}
	if err := a.api(http.MethodPost, "/api/projects", map[string]string{"name": fmt.Sprintf("Writers %d", a.walk)}, &r); err != nil {
		return err
	}
	a.slug = r.Project.Slug
	if err := a.api(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/MEMORY.md", map[string]string{"text": pdwV0}, nil); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	row, err := a.s.CreateSession(ctx, a.cwd, "")
	cancel()
	if err != nil {
		return err
	}
	a.sid, a.turns, a.held = row.ID, 0, ""
	a.sessions = append(a.sessions, a.sid)
	if err := a.api(http.MethodPost, "/api/sessions/"+a.sid+"/project", map[string]string{"project": a.slug}, nil); err != nil {
		return err
	}
	// The child was spawned before it was filed; the project directory
	// reaches it at its next start.
	if err := a.api(http.MethodPost, "/api/sessions/"+a.sid+"/archive", nil, nil); err != nil {
		return err
	}
	if err := a.api(http.MethodPost, "/api/sessions/"+a.sid+"/unarchive", nil, nil); err != nil {
		return err
	}
	if err := a.turn(); err != nil {
		return fmt.Errorf("init turn: %w", err)
	}
	a.edited, a.edErr, a.draftText = false, false, ""
	if err := a.readFiles(); err != nil {
		return err
	}
	a.gate.reset()
	return nil
}

// Cleanup releases a turn a failed step left held and archives the
// walk's session, which ends its child: walks do not pile up processes.
func (a *pdwAdapter) Cleanup() error {
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
	}
	if a.sid == "" {
		return nil
	}
	return a.api(http.MethodPost, "/api/sessions/"+a.sid+"/archive", nil, nil)
}

func (a *pdwAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

func (a *pdwAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return a.state(v), nil
}

// pdwShown is the spec's view(): a link reads as its target.
func pdwShown(mem string) string {
	if mem == "link" {
		return "host"
	}
	return mem
}

// step gates an action on the spec's require, read off the view.
func (a *pdwAdapter) step(require func(v pdwView) bool, do func(v pdwView) error) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(v)) {
		return nil
	}
	return do(v)
}

// turn is one model turn of the walk's session: a held turn so the row
// is seen running, then released and seen done.
func (a *pdwAdapter) turn() error {
	a.turns++
	name := fmt.Sprintf("pdw-%s-%s-%03d", a.slug, a.sid, a.turns)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "noted"})
	a.held = name
	ctx, cancel := actionCtx()
	err := a.s.Prompt(ctx, a.sid, fmt.Sprintf("turn %d", a.turns))
	cancel()
	if err != nil {
		return err
	}
	if err := pdwWaitFile(filepath.Join(a.dir, name+".taken")); err != nil {
		return fmt.Errorf("turn %s taken: %w", name, err)
	}
	if _, err := waitRow(a.s, a.sid, "the turn to run", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	control.Release(a.t, a.dir, name)
	a.held = ""
	_, err = waitRow(a.s, a.sid, "the turn to finish", func(r serve.Row) bool { return r.Status == serve.StatusDone })
	return err
}

func pdwWaitFile(p string) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(p); err == nil {
			return nil
		}
		time.Sleep(20 * time.Millisecond)
	}
	return fmt.Errorf("%s never appeared", p)
}

func (a *pdwAdapter) PersonEdits() error {
	return a.step(func(v pdwView) bool { return !v.draft && v.fm != "host" }, func(pdwView) error {
		a.edits++
		a.draftText = a.fmText + fmt.Sprintf(pdwPerson, a.edits)
		a.edited = true
		return nil
	})
}

// PersonSaves is Save: the whole draft, no base version. Success
// re-reads the files; a 404 keeps the draft beside the error.
func (a *pdwAdapter) PersonSaves() error {
	return a.step(func(v pdwView) bool { return v.draft }, func(pdwView) error {
		text := a.draftText
		if a.mergeOnSave {
			if b, err := os.ReadFile(a.memPath()); err == nil && strings.Contains(string(b), "# Remembered") && !strings.Contains(text, "# Remembered") {
				text += pdwFact
			}
		}
		err := a.api(http.MethodPut, "/api/projects/"+a.slug+"/orb/files/MEMORY.md", map[string]string{"text": text}, nil)
		var ae *servetest.APIError
		if errors.As(err, &ae) && ae.Status == http.StatusNotFound {
			a.edErr = true
			return nil
		}
		if err != nil {
			return err
		}
		a.edited, a.draftText = false, ""
		return a.readFiles()
	})
}

// ReopenPage leaves the project and comes back: ProjectView remounts
// and reads the files again.
func (a *pdwAdapter) ReopenPage() error {
	return a.step(func(v pdwView) bool { return v.dir && !v.draft && v.fm != pdwShown(v.mem) }, func(pdwView) error {
		return a.readFiles()
	})
}

// AgentRemembers: the file tools (or a guest heredoc, or `bough
// project write`) add the fact to MEMORY.md; nothing tells the page.
func (a *pdwAdapter) AgentRemembers() error {
	return a.step(func(v pdwView) bool { return v.dir && (v.mem == "v0" || v.mem == "edit") }, func(pdwView) error {
		b, err := os.ReadFile(a.memPath())
		if err != nil {
			return err
		}
		return os.WriteFile(a.memPath(), append(b, pdwFact...), 0o644)
	})
}

// GuestLinksMemory: a guest shell, through the read-write project
// mount, replaces MEMORY.md with a symlink to a host file.
func (a *pdwAdapter) GuestLinksMemory() error {
	return a.step(func(v pdwView) bool { return v.dir && v.mem != "link" }, func(pdwView) error {
		if err := os.Remove(a.memPath()); err != nil {
			return err
		}
		return os.Symlink(a.host, a.memPath())
	})
}

// DeleteProject is DELETE /api/projects/<slug> from another tab.
func (a *pdwAdapter) DeleteProject() error {
	return a.step(func(v pdwView) bool { return v.dir }, func(pdwView) error {
		return a.api(http.MethodDelete, "/api/projects/"+a.slug, nil, nil)
	})
}

// TurnStarts is a message to the session: context-md reads MEMORY.md
// as the turn starts.
func (a *pdwAdapter) TurnStarts() error {
	return a.step(func(v pdwView) bool { return v.inj != pdwShown(v.mem) }, func(pdwView) error {
		return a.turn()
	})
}

var pdwActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"PersonEdits":      action((*pdwAdapter).PersonEdits),
	"PersonSaves":      action((*pdwAdapter).PersonSaves),
	"ReopenPage":       action((*pdwAdapter).ReopenPage),
	"AgentRemembers":   action((*pdwAdapter).AgentRemembers),
	"GuestLinksMemory": action((*pdwAdapter).GuestLinksMemory),
	"DeleteProject":    action((*pdwAdapter).DeleteProject),
	"TurnStarts":       action((*pdwAdapter).TurnStarts),
}, "": {
	// fizz links a state with nothing enabled to itself as "end" (a
	// deleted project whose editor shows a link's target): nothing
	// happens in serve, and the gate closes so the walk stops there.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*pdwAdapter).gate.pass(false)
		return nil, nil
	},
}}

// projectDefinitionWritersHistory reads what a transcript can say about
// this flow: that its first turn finished, which is Init's turn sending
// v0. Nothing past it is a path the transcript can show. A later turn
// is a TurnStarts, but every writer that enables one (the page, an
// agent's file write, a guest's symlink, Delete) leaves nothing in the
// session's history, and what the turn sent is the engine's store, not
// the history. walkPath compares every step's state with the spec's;
// this checks the record a session keeps agrees with where walks start.
func projectDefinitionWritersHistory(entries []history.Entry) []tracecheck.Step {
	inj := "no finished first turn"
	inputs := 0
	for _, e := range entries {
		if e.Kind == "input" && e.Data["reason"] != "notice" {
			inputs++
		}
		if e.Kind == "done" && inputs == 1 {
			inj = "v0"
			break
		}
	}
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Project#0.inj": inj}}}
}

func init() { historyProjections["project_definition_writers"] = projectDefinitionWritersHistory }

// walkPath drives one generated path through the adapter and compares
// the state it reads with the path's after every step.
func (a *pdwAdapter) walkPath(trace []tracecheck.Step) (err error) {
	if err := a.Init(); err != nil {
		return err
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	var done []string
	for i, s := range trace {
		name := strings.TrimPrefix(s.Action, "Project#0.")
		if i > 0 {
			done = append(done, name)
		}
		if i > 0 && name != "end" {
			f, ok := pdwActions["Project"][name]
			if !ok {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			if _, err := f(a, nil); err != nil {
				return fmt.Errorf("step %d (%s) of %v: %w", i, name, done, err)
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s) of %v: the adapter reads it as not enabled", i, name, done)
			}
		}
		have, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s) of %v: state: %w", i, name, done, err)
		}
		if diff := pmtDiff(s.State, have); diff != "" {
			return fmt.Errorf("step %d (%s) of %v: state differs from the spec's:%s", i, name, done, diff)
		}
	}
	return nil
}

func pdwPaths(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("project_definition_writers", cover)
	if err != nil {
		t.Fatal(err)
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		t.Fatal(err)
	}
	var out [][]tracecheck.Step
	for _, p := range file.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// TestProjectDefinitionWritersPaths walks every generated path (every
// settled state; MODEL_COVER=transitions: every transition) against
// real serves, then replays every walk's transcript on the graph.
func TestProjectDefinitionWritersPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	paths := pdwPaths(t, envCover())
	g, err := tracecheck.Load(fizzCheck(t, "project_definition_writers"))
	if err != nil {
		t.Fatal(err)
	}
	const shards = 3
	for n := range shards {
		t.Run(fmt.Sprintf("serve%d", n), func(t *testing.T) {
			t.Parallel()
			a := newPDWAdapter(t)
			failed := 0
			for i := n; i < len(paths); i += shards {
				if err := a.walkPath(paths[i]); err != nil {
					t.Errorf("path %d: %v", i, err)
					if failed++; failed >= 5 {
						t.Fatal("five paths failed on this serve; stopping")
					}
				}
			}
			for _, id := range a.sessions {
				checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectDefinitionWritersHistory)
			}
			if len(a.sessions) == 0 {
				t.Error("no transcripts for the trace check")
			}
			t.Logf("walked %d paths, trace-checked %d transcripts", (len(paths)-n+shards-1)/shards, len(a.sessions))
		})
	}
}

// A page that keeps an agent's fact when it saves a draft made before
// the fact was written is the product the spec wishes for, not the one
// it models: the walk over every transition must fail at the save that
// loses the fact. It stops at the first path that does.
func TestProjectDefinitionWritersPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPDWAdapter(t)
	a.mergeOnSave = true
	for i, p := range pdwPaths(t, tracecheck.CoverTransitions) {
		if err := a.walkPath(p); err != nil {
			t.Logf("path %d caught it: %v", i, err)
			return
		}
	}
	t.Fatal("every path passed with a save that keeps the agent's fact; the walk is not checking state")
}
