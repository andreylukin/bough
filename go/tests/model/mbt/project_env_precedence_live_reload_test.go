//go:build !windows

package mbt

import (
	"fmt"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/project_env_precedence_live_reload.fizz against a real serve:
// one local session, and the project it is filed under, whose directory
// only a fresh process start ever picks up. EditProjectFile files the
// session into generation 0 ("" - no project), 1 ("p1") or 2 ("p2") by
// slug, monotonically, matching the spec's fileVer.
//
// There is no separate cached "row" in front of this in internal/serve:
// AssignProject writes meta.json in memory and Row.Project reads it back
// at once (supervisor.go's projectEnv comment: derived "at EVERY child
// start", never pushed to a live one). So fileVer and rowLoaded are read
// off the very same live field, Row.Project, and KernelHotReload's
// require (rowLoaded != fileVer) is real but never true in a walk that
// never overlaps a write with a read: it is here for the spec's
// generality, and the adapter is honest that it never fires for real.
// childEnv is the generation the running child actually started with,
// Row.StartedIn at the moment SpawnSession's turn takes; the adapter
// freezes it there (like exampleAdapter's viewing), because serve itself
// clears StartedIn once no child runs, while the spec keeps the value.
//
// This flow has no container orb (that is project-mode sessions only,
// plugins/orb, not a local one filed into a project for MEMORY.md). Its
// OpenOrb instead cross-checks two independently derived real signals
// the header comment's own cited files describe: Row.StartedIn (the
// API's answer) against the session's own "meta" history entry naming
// the directory it started in (the child's self-report, exactly what
// projectenv_test.go's startedWith reads). Either one drifting from the
// frozen childEnv is a real product bug.
const pevlrConfig = controlConfig

// pevlrSlugs maps the spec's fileVer/childEnv/orbIdentity generation (0,
// 1, 2) to a project slug ("", "p1", "p2"); index 0 is "no project".
var pevlrSlugs = [3]string{"", "p1", "p2"}

func genOfSlug(slug string) (int, error) {
	for i, s := range pevlrSlugs {
		if s == slug {
			return i, nil
		}
	}
	return 0, fmt.Errorf("project env: unknown slug %q", slug)
}

// genOfDir recovers a generation from a BOUGH_PROJECT_DIR-shaped path,
// which ends in "/projects/<slug>" (projectdef.Root(home)/<slug>): a
// suffix match needs no home, which the history projection has none of.
func genOfDir(dir string) (int, error) {
	if dir == "" {
		return 0, nil
	}
	for i := 1; i < len(pevlrSlugs); i++ {
		if strings.HasSuffix(dir, "/projects/"+pevlrSlugs[i]) {
			return i, nil
		}
	}
	return 0, fmt.Errorf("project env: dir %q matches no known project", dir)
}

type projectEnvAdapter struct {
	t   *testing.T
	s   *servetest.Server
	dir string // llm-control's queue

	gate gate

	id       string
	archived bool
	turn     int
	held     string // the wake turn in flight, "" when none

	// Frozen at SpawnSession and OpenOrb respectively, like
	// exampleAdapter's viewing: the real server clears the fields these
	// come from once no child runs, but the spec keeps what it saw.
	childEnv    int
	orbOpen     bool
	orbIdentity int

	ids []string

	// wrongOrbUsesLive is the deliberate wiring bug
	// TestProjectEnvPrecedenceLiveReloadCatchesWrongAdapter injects:
	// OpenOrb reports the currently assigned project instead of the one
	// the running child actually started with.
	wrongOrbUsesLive bool
}

func newProjectEnvAdapter(t *testing.T) *projectEnvAdapter {
	s := servetest.Start(t, servetest.Options{Config: pevlrConfig})
	for i := 1; i < len(pevlrSlugs); i++ {
		if _, err := projectdef.CreateEmpty(s.Home, pevlrSlugs[i], pevlrSlugs[i]); err != nil {
			t.Fatalf("create project %s: %v", pevlrSlugs[i], err)
		}
	}
	return &projectEnvAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts each walk on a fresh, unassigned, not-yet-started local
// session in the same serve: CreateSession with no prompt leaves it
// idle, matching the spec's Init (running = False).
func (a *projectEnvAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.archived, a.held = row.ID, false, ""
	a.childEnv, a.orbOpen, a.orbIdentity = 0, false, 0
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup lets a wake turn the walk left running finish, so its child is
// not holding a request while the next walk queues its own.
func (a *projectEnvAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err := waitRow(a.s, a.id, "the held wake turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

func (a *projectEnvAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// row is the session's current row, the one live source fileVer,
// rowLoaded and running all read.
func (a *projectEnvAdapter) row() (serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	return row, err
}

// GetState is the Project role's state. fileVer and rowLoaded are the
// same live read (Row.Project): there is no separate cached row for a
// local session, so they can never be caught disagreeing here. running
// is Row.Live. childEnv and orbIdentity are frozen (see the type
// comment).
func (a *projectEnvAdapter) GetState() (map[string]any, error) {
	row, err := a.row()
	if err != nil {
		return nil, err
	}
	fileVer, err := genOfSlug(row.Project)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"fileVer":     fileVer,
		"rowLoaded":   fileVer,
		"running":     row.Live,
		"childEnv":    a.childEnv,
		"orbOpen":     a.orbOpen,
		"orbIdentity": a.orbIdentity,
	}, nil
}

// Each action first asks the gate with the spec's require: a walk's
// validation ends at its first disabled action, so the rest is skipped.

// EditProjectFile moves the session to the next generation's project,
// same as SetSecret or a hand edit: an identity/membership change,
// allowed whether or not a session is running.
func (a *projectEnvAdapter) EditProjectFile() error {
	row, err := a.row()
	if err != nil {
		return err
	}
	cur, err := genOfSlug(row.Project)
	if err != nil {
		return err
	}
	if !a.gate.pass(cur < len(pevlrSlugs)-1) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.API(ctx, "POST", "/api/sessions/"+a.id+"/project", map[string]string{"project": pevlrSlugs[cur+1]}, nil)
}

// KernelHotReload's require (rowLoaded != fileVer) is real, but reading
// both off the very same live field (Row.Project) means it is never true
// in a walk with no concurrent writer: this action is here for the
// spec's generality, honestly never firing over a real serve. See the
// type comment.
func (a *projectEnvAdapter) KernelHotReload() error {
	row, err := a.row()
	if err != nil {
		return err
	}
	fileVer, err := genOfSlug(row.Project)
	if err != nil {
		return err
	}
	// rowLoaded is read the same way, so this is always equal; the gate
	// call is kept so a real divergence, if one ever appeared, would be
	// reported by the runner rather than silently accepted.
	a.gate.pass(fileVer != fileVer)
	return nil
}

// SpawnSession is a brand-new process for this session: unarchive if
// killed, then wake it and wait for the child to be live. childEnv is
// frozen here from Row.StartedIn the instant the child is confirmed up.
func (a *projectEnvAdapter) SpawnSession() error {
	row, err := a.row()
	if err != nil {
		return err
	}
	if !a.gate.pass(!row.Live) {
		return nil
	}
	if a.archived {
		ctx, cancel := actionCtx()
		defer cancel()
		if _, err := a.s.Unarchive(ctx, a.id); err != nil {
			return err
		}
		a.archived = false
	}
	a.turn++
	name := fmt.Sprintf("e%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "woke " + name})
	ctx, cancel := actionCtx()
	if err := a.s.Prompt(ctx, a.id, "wake "+name); err != nil {
		cancel()
		return err
	}
	cancel()
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	a.held = name
	live, err := waitRow(a.s, a.id, "the child to start", func(r serve.Row) bool { return r.Live })
	if err != nil {
		return err
	}
	a.childEnv, err = genOfSlug(live.StartedIn)
	if err != nil {
		return err
	}
	a.orbOpen, a.orbIdentity = false, 0
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	_, err = waitRow(a.s, a.id, "the wake turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

// OpenOrb reads which directory the running process mounted, from two
// independent real signals: Row.StartedIn (the API's answer) and the
// child's own "meta" history entry (its self-report, written once at
// its start). Both must agree with the frozen childEnv, or this is a
// real product bug.
func (a *projectEnvAdapter) OpenOrb() error {
	row, err := a.row()
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live && !a.orbOpen) {
		return nil
	}
	fromRow, err := genOfSlug(row.StartedIn)
	if err != nil {
		return err
	}
	fromChild, err := a.lastStartedDir()
	if err != nil {
		return err
	}
	if a.wrongOrbUsesLive {
		g, err := genOfSlug(row.Project)
		if err != nil {
			return err
		}
		fromRow = g
	} else if fromRow != fromChild {
		return fmt.Errorf("orb identity: Row.StartedIn says generation %d, the child's own meta entry says %d", fromRow, fromChild)
	}
	a.orbOpen, a.orbIdentity = true, fromRow
	return nil
}

// lastStartedDir is the project_dir the child's most recent "meta" entry
// recorded, as a generation: startedWith in projectenv_test.go reads the
// same field, from the same file, by a different path (disk, not HTTP).
func (a *projectEnvAdapter) lastStartedDir() (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	_, entries, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return 0, err
	}
	dir, seen := "", false
	for _, e := range entries {
		if e.Kind != "meta" {
			continue
		}
		if v, ok := e.Data["project_dir"]; ok {
			dir, _ = v.(string)
			seen = true
		}
	}
	if !seen {
		return 0, fmt.Errorf("no meta entry with project_dir yet")
	}
	return genOfDir(dir)
}

// KillSession ends the process; its orb goes with it. childEnv and
// orbIdentity stay as they were: the spec never resets them here, only
// SpawnSession does (a fresh generation, or none at all).
func (a *projectEnvAdapter) KillSession() error {
	row, err := a.row()
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live) {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	a.archived = true
	a.orbOpen = false
	return nil
}

var projectEnvActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"EditProjectFile": action((*projectEnvAdapter).EditProjectFile),
	"KernelHotReload": action((*projectEnvAdapter).KernelHotReload),
	"SpawnSession":    action((*projectEnvAdapter).SpawnSession),
	"OpenOrb":         action((*projectEnvAdapter).OpenOrb),
	"KillSession":     action((*projectEnvAdapter).KillSession),
}}

// Every step of a walk is a real turn through a real serve, so the
// default run is short.
func projectEnvOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// projectEnvHistory reads the abstract trace off a transcript.
// EditProjectFile, KernelHotReload and OpenOrb leave nothing in
// history (membership lives in meta.json, and an orb read has no
// side effect); each "meta" entry with a project_dir is a
// SpawnSession, and each input/done pair inside it is the wake turn
// SpawnSession itself opens and closes. A meta entry's project_dir
// gives childEnv directly, the same signal OpenOrb itself reads, so
// checking is on running and childEnv alone.
func projectEnvHistory(entries []history.Entry) []tracecheck.Step {
	st := func(running bool, childEnv int) map[string]any {
		return map[string]any{"Project#0.running": running, "Project#0.childEnv": childEnv}
	}
	steps := []tracecheck.Step{{Action: "Init", State: st(false, 0)}}
	for _, e := range entries {
		if e.Kind != "meta" {
			continue
		}
		v, ok := e.Data["project_dir"]
		if !ok {
			continue
		}
		dir, _ := v.(string)
		gen, err := genOfDir(dir)
		if err != nil {
			// An unrecognised project_dir: keep it out of the trace
			// rather than mis-project a generation; the trace check
			// then fails loudly on the gap instead of silently.
			gen = -1
		}
		steps = append(steps, tracecheck.Step{Action: "Project#0.SpawnSession", State: st(true, gen)})
	}
	return steps
}

func init() { historyProjections["project_env_precedence_live_reload"] = projectEnvHistory }

func TestProjectEnvPrecedenceLiveReload(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newProjectEnvAdapter(t)
	if err := runMBT(t, "project_env_precedence_live_reload", a, projectEnvActions, projectEnvOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "project_env_precedence_live_reload"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), projectEnvHistory)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter reports the orb's identity as whatever project
// the session is CURRENTLY assigned to instead of the one its running
// child actually started with — a page that trusted the live
// assignment instead of Row.StartedIn would show the wrong MEMORY.md —
// and a walk that edits the file after spawning must catch it.
func TestProjectEnvPrecedenceLiveReloadCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newProjectEnvAdapter(t)
	a.wrongOrbUsesLive = true
	if err := runMBT(t, "project_env_precedence_live_reload", a, projectEnvActions, projectEnvOptions()); err == nil {
		t.Fatal("a run whose orb reports the live project instead of the started one passed; the runner is not checking state")
	}
}
