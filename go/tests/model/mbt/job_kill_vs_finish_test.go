//go:build !windows

package mbt

import (
	"errors"
	"fmt"
	"net/http"
	"net/url"
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

// specs/job_kill_vs_finish.fizz against a real serve: one background
// bash job (started by a model turn, then the session is idle) that the
// Work dialog's kill POST, the job's own exit and the child's death
// (Stop) race to settle. The spec's claim is that the job's typed
// finished entry is written exactly once whoever wins.
//
// job and finished are read off the typed "job" entries in the history
// file; child is the row's Live; kill_reply is the last kill POST's
// status, kills and reaped are the adapter's own record.

const jkMaxKills = 2

type jkAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id     string
	ids    []string
	jobDir string
	walk   int
	turn   int

	kills     int
	killReply string
	reaped    bool
	how       string // how the job settled, by the action that drove it: a clean Stop makes the child itself kill the job, so the file cannot tell a kill from the reap

	// exitAsKill is the deliberate bug of the wrong-adapter test: the
	// job's own exit is made real as a kill.
	exitAsKill bool
}

func newJKAdapter(t *testing.T) *jkAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig + "- id: loop\n  plugin: engine-unreal\n"})
	return &jkAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *jkAdapter) script() string {
	return fmt.Sprintf(`D=%q; P=$PPID
while [ ! -e "$D/exit" ]; do
  kill -0 "$P" 2>/dev/null || exit 3
  sleep 0.02
done`, a.jobDir)
}

// Init starts a fresh session whose model turn has started one
// background job, and waits for the turn to end: idle, child alive, job
// running.
func (a *jkAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.walk++
	a.jobDir = filepath.Join(a.s.Root, fmt.Sprintf("jkjob%04d", a.walk))
	if err := os.MkdirAll(a.jobDir, 0o755); err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.kills, a.killReply, a.reaped, a.how = 0, "none", false, ""
	a.gate.reset()

	a.turn++
	first, second := fmt.Sprintf("jk%04da", a.turn), fmt.Sprintf("jk%04db", a.turn)
	control.Queue(a.t, a.dir, first, control.Turn{Mode: "call", Tool: "bash",
		Args: map[string]any{"command": a.script(), "background": true, "timeout": "300s"}})
	control.Queue(a.t, a.dir, second, control.Turn{Mode: "ok", Text: "started"})
	if err := a.s.Prompt(ctx, a.id, "start the job "+first); err != nil {
		return err
	}
	return a.waitFor("the job to start and the turn to end", func(r serve.Row, e []history.Entry) bool {
		return r.Status != serve.StatusRunning && r.Live && len(jkTyped(e, "started")) == 1
	})
}

func (a *jkAdapter) Cleanup() error {
	ctx, cancel := actionCtx()
	defer cancel()
	var e *servetest.APIError
	if _, err := a.s.Archive(ctx, a.id); errors.As(err, &e) && e.Status == http.StatusNotFound {
		return nil
	} else if err != nil {
		return err
	}
	return a.waitFor("the archived child to exit", func(r serve.Row, _ []history.Entry) bool { return !r.Live })
}

func (a *jkAdapter) read() (serve.Row, []history.Entry, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	var e *servetest.APIError
	if errors.As(err, &e) && e.Status == http.StatusNotFound {
		return serve.Row{}, nil, nil
	} else if err != nil {
		return row, nil, err
	}
	ents, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	return row, ents, err
}

func (a *jkAdapter) waitFor(what string, ok func(serve.Row, []history.Entry) bool) error {
	deadline := time.Now().Add(actionTimeout)
	for {
		row, ents, err := a.read()
		if err == nil && ok(row, ents) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting %s for %s: row %s live=%v, %d entries (err %v)", actionTimeout, what, row.Status, row.Live, len(ents), err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// jkTyped is the job entries of one typed event.
func jkTyped(ents []history.Entry, event string) []history.Entry {
	var out []history.Entry
	for _, e := range ents {
		if e.Kind == "job" && e.Data["event"] == event {
			out = append(out, e)
		}
	}
	return out
}

func (a *jkAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Job", Index: 0}: a}, nil
}

func (a *jkAdapter) GetState() (map[string]any, error) {
	row, ents, err := a.read()
	if err != nil {
		return nil, err
	}
	child := "gone"
	if row.Live {
		child = "alive"
	}
	job, fin := "running", jkTyped(ents, "finished")
	if len(fin) > 0 {
		job = a.how
	}
	return map[string]any{
		"child": child, "job": job, "finished": len(fin),
		"kill_reply": a.killReply, "kills": a.kills, "reaped": a.reaped,
	}, nil
}

// jkHow is how a finished entry says the job ended.
func jkHow(e history.Entry) string {
	if e.Data["stopped"] == true {
		return "stopped"
	}
	if n, ok := e.Data["exit"].(float64); ok && n == 0 {
		return "exited"
	}
	return "killed"
}

func (a *jkAdapter) postKill() (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+"/api/sessions/"+url.PathEscape(a.id)+"/jobs/1/kill", nil)
	if err != nil {
		return 0, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	resp.Body.Close()
	return resp.StatusCode, nil
}

func (a *jkAdapter) finished() int {
	_, ents, _ := a.read()
	return len(jkTyped(ents, "finished"))
}

func (a *jkAdapter) KillRequest() error {
	if !a.gate.pass(a.kills < jkMaxKills) {
		return nil
	}
	_, before, err := a.read()
	if err != nil {
		return err
	}
	wasRunning := len(jkTyped(before, "finished")) == 0
	code, err := a.postKill()
	if err != nil {
		return err
	}
	a.kills++
	a.killReply = fmt.Sprint(code)
	if code == http.StatusOK && wasRunning {
		a.how = "killed"
		// The child kills the job asynchronously; the kill's finished
		// entry is what settles it.
		return a.waitFor("the kill's finished entry", func(_ serve.Row, e []history.Entry) bool {
			return len(jkTyped(e, "finished")) >= 1
		})
	}
	return nil
}

func (a *jkAdapter) JobExits() error {
	row, ents, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live && len(jkTyped(ents, "finished")) == 0) {
		return nil
	}
	a.how = "exited"
	if a.exitAsKill {
		code, err := a.postKill()
		if err != nil {
			return err
		}
		a.kills++ // the wrong adapter's kill is not the spec's; the mismatch is the point
		a.killReply = fmt.Sprint(code)
	} else if err := os.WriteFile(filepath.Join(a.jobDir, "exit"), nil, 0o644); err != nil {
		return err
	}
	return a.waitFor("the job's finished entry", func(_ serve.Row, e []history.Entry) bool {
		return len(jkTyped(e, "finished")) >= 1
	})
}

// ChildDies is Stop: the child exits, and serve's reap ends a job the
// file still shows running.
func (a *jkAdapter) ChildDies() error {
	row, ents, err := a.read()
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live) {
		return nil
	}
	if len(jkTyped(ents, "finished")) == 0 {
		a.how = "stopped"
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Interrupt(ctx, a.id); err != nil {
		return fmt.Errorf("stop: %w", err)
	}
	a.reaped = true
	return a.waitFor("the child to exit and its job to be settled", func(r serve.Row, e []history.Entry) bool {
		return !r.Live && len(jkTyped(e, "finished")) >= 1
	})
}

var jkActions = map[string]map[string]fmbt.ActionFunc{"Job": {
	"KillRequest": action((*jkAdapter).KillRequest),
	"JobExits":    action((*jkAdapter).JobExits),
	"ChildDies":   action((*jkAdapter).ChildDies),
}, "": {
	// deadlock_detection is off, so the runner offers a role-less "end";
	// declined like any disabled pick.
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*jkAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func jkOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 5, "max-parallel-runs": 0}
}

// jkHistory reads the abstract trace off a transcript: the job's one
// finished entry is the action that settled it. Requests that change
// nothing leave nothing in history.
func jkHistory(entries []history.Entry) []tracecheck.Step {
	st := func(job string) map[string]any { return map[string]any{"Job#0.job": job, "Job#0.finished": 1} }
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Job#0.job": "running", "Job#0.finished": 0}}}
	quit := false
	for _, e := range entries {
		if e.Kind == "job" && e.Data["event"] == nil {
			if txt, _ := e.Data["text"].(string); strings.Contains(txt, "killed when bough quit") {
				quit = true
			}
		}
	}
	for _, e := range entries {
		if e.Kind != "job" || e.Data["event"] != "finished" {
			continue
		}
		how := jkHow(e)
		if quit || how == "stopped" {
			how = "stopped"
		}
		switch how {
		case "stopped":
			steps = append(steps, tracecheck.Step{Action: "Job#0.ChildDies", State: st("stopped")})
		case "exited":
			steps = append(steps, tracecheck.Step{Action: "Job#0.JobExits", State: st("exited")})
		default:
			steps = append(steps, tracecheck.Step{Action: "Job#0.KillRequest", State: st("killed")})
		}
	}
	return steps
}

func init() { historyProjections["job_kill_vs_finish"] = jkHistory }

func TestJobKillVsFinish(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newJKAdapter(t)
	if err := runMBT(t, "job_kill_vs_finish", a, jkActions, jkOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "job_kill_vs_finish"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), jkHistory)
	}
}

func TestJobKillVsFinishCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newJKAdapter(t)
	a.exitAsKill = true
	if err := runMBT(t, "job_kill_vs_finish", a, jkActions, jkOptions()); err == nil {
		t.Fatal("a run whose JobExits is a kill passed; the runner is not checking state")
	}
}
