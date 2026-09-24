//go:build !windows

package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
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

// specs/archive_unarchive.fizz against a real serve: one web session
// that may start one background agent, archived and unarchived the way
// the page does it (archiveRow's confirm or choice dialog, then POST
// /archive with or without stopChildren).
//
// The parent's tools.spawn({background}) is a POST /api/sessions with
// spawnedBy (plugins/workers/background.go); the adapter sends that same
// request itself instead of scripting a tool call through the model, so
// the spawn lands exactly as serve sees it from the tool. A queued agent
// (SpawnQueued) needs the running cap full: a "blocker" agent of a
// separate filler session holds the one slot a maxRunning of 1 allows,
// and AgentStart is that blocker finishing.

type archiveUnarchiveAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	filler string // parent of the blockers; kept down, so never woken
	id     string // this walk's session
	dialog string // "none", "confirm" or "choice": the page's own state
	agent  string // the agent's id, "" before Spawn
	held   string // the agent's turn in flight, "" when none
	block  string // the blocker's held turn, "" when none
	turn   int
	ids    []string
	ran    map[string]int // steps run per action, for the log

	// The deliberate bugs the wrong-adapter tests inject.
	// alwaysChoice: Archive asks "Stop its agents too?" with no agent,
	// a first step the random walks take about one time in eleven.
	// stopIsArchiveOnly: "Stop and archive" sends no stopChildren, so a
	// running agent keeps running; only the paths walk reaches it.
	alwaysChoice, stopIsArchiveOnly bool
}

func newArchiveUnarchiveAdapter(t *testing.T) *archiveUnarchiveAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &archiveUnarchiveAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	ctx, cancel := actionCtx()
	defer cancel()
	name := a.next()
	control.Queue(t, a.dir, name, control.Turn{Mode: "ok", Text: "filler"})
	row, err := s.CreateSession(ctx, s.Dir(t, "filler"), "filler "+name)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := waitRow(s, row.ID, "the filler's turn", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		t.Fatal(err)
	}
	a.filler = row.ID
	if err := a.downFiller(ctx); err != nil {
		t.Fatal(err)
	}
	return a
}

// next is a turn name unique across walks: the control queue is shared
// and taken in lexical order.
func (a *archiveUnarchiveAdapter) next() string {
	a.turn++
	return fmt.Sprintf("t%05d", a.turn)
}

// post is POST /api/sessions/{id}/{verb} as the page sends it; body nil
// is a bare POST (archive's "no stopChildren").
func (a *archiveUnarchiveAdapter) post(ctx context.Context, id, verb string, body, out any) error {
	return a.call(ctx, "/api/sessions/"+url.PathEscape(id)+"/"+verb, body, out)
}

func (a *archiveUnarchiveAdapter) call(ctx context.Context, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, a.s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("POST %s: %d %s", path, resp.StatusCode, raw)
	}
	if out != nil {
		return json.Unmarshal(raw, out)
	}
	return nil
}

func (a *archiveUnarchiveAdapter) children(ctx context.Context, id string) ([]serve.Row, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+"/api/sessions/"+url.PathEscape(id)+"/children", nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	var r struct {
		Children []serve.Row `json:"children"`
	}
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("children of %s: %d %s", id, resp.StatusCode, raw)
	}
	if err := json.Unmarshal(raw, &r); err != nil {
		return nil, fmt.Errorf("children of %s: %w: %s", id, err, raw)
	}
	return r.Children, nil
}

// spawn is the tools.spawn({background}) request.
func (a *archiveUnarchiveAdapter) spawn(ctx context.Context, parent, prompt string, maxRunning int) (id string, queued bool, err error) {
	var r struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	err = a.call(ctx, "/api/sessions", map[string]any{"prompt": prompt, "spawnedBy": parent, "maxRunning": maxRunning}, &r)
	return r.Session.ID, r.Queued, err
}

func (a *archiveUnarchiveAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	// Create leases a child at once; the spec starts from a session with
	// none (as after a serve restart). Archiving kills it and
	// unarchiving respawns nothing, which is also what the spec says.
	// Every verb 404s until the child has written its history file.
	if _, err := waitRow(a.s, row.ID, "the new session's file", func(serve.Row) bool { return true }); err != nil {
		return err
	}
	if err := a.post(ctx, row.ID, "archive", nil, nil); err != nil {
		return err
	}
	if err := a.post(ctx, row.ID, "unarchive", nil, nil); err != nil {
		return err
	}
	a.id, a.dialog, a.agent, a.held = row.ID, "none", "", ""
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup lets every held turn end and then ends every process the walk
// started (the parent, its agent, the blocker), so a hundred walks do
// not leave hundreds of idle children behind or a slot taken.
func (a *archiveUnarchiveAdapter) Cleanup() error {
	if a.block != "" {
		if err := a.releaseBlocker(); err != nil {
			return err
		}
	}
	if a.held != "" {
		control.Release(a.t, a.dir, a.held)
		a.held = ""
		if _, err := waitRow(a.s, a.agent, "the agent's held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
			return err
		}
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.post(ctx, a.id, "archive", map[string]bool{"stopChildren": true}, nil); err != nil {
		return err
	}
	return a.downFiller(ctx)
}

// downFiller ends the filler's child and its blockers and leaves it
// unarchived: a blocker's report then lands in its file instead of
// waking a turn that would take the next queued one, and serve still
// takes spawns from it (it refuses them from an archived parent).
func (a *archiveUnarchiveAdapter) downFiller(ctx context.Context) error {
	if err := a.post(ctx, a.filler, "archive", map[string]bool{"stopChildren": true}, nil); err != nil {
		return err
	}
	return a.post(ctx, a.filler, "unarchive", nil, nil)
}

func (a *archiveUnarchiveAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

type auState struct {
	history, live, archived, listed bool
	agent                           string
	row                             serve.Row
}

// state reads every server-side field: history, live and archived off
// the session's row, listed off the default list, agent off the
// children listing (the agent's own row).
func (a *archiveUnarchiveAdapter) state() (auState, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return auState{}, err
	}
	rows, err := a.s.ListSessions(ctx, false)
	if err != nil {
		return auState{}, err
	}
	kids, err := a.children(ctx, a.id)
	if err != nil {
		return auState{}, err
	}
	st := auState{history: !row.Empty, live: row.Live, archived: row.Archived, row: row,
		listed: slices.ContainsFunc(rows, func(r serve.Row) bool { return r.ID == a.id })}
	switch len(kids) {
	case 0:
		st.agent = "none"
	case 1:
		st.agent = agentWord(kids[0].Status)
	default:
		st.agent = fmt.Sprintf("%d agents", len(kids))
	}
	return st, nil
}

// agentWord is the spec's agent value for the agent's row status. A
// stop reads "stopped", or "interrupted" when the kill landed before the
// cancel was recorded: both are the spec's stopped. Anything else the
// spec does not name passes through, so it shows up as a mismatch.
func agentWord(s serve.Status) string {
	if s == serve.StatusInterrupted {
		return "stopped"
	}
	return string(s)
}

func (a *archiveUnarchiveAdapter) GetState() (map[string]any, error) {
	st, err := a.state()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"history": st.history, "live": st.live, "archived": st.archived,
		"listed": st.listed, "dialog": a.dialog, "agent": st.agent,
	}, nil
}

// enabled reads the require off the server's view.
func (a *archiveUnarchiveAdapter) enabled(require func(auState) bool) (auState, bool) {
	if a.gate.off {
		return auState{}, false
	}
	st, err := a.state()
	if err != nil {
		a.gate.off = true
		return st, false
	}
	return st, a.gate.pass(require(st))
}

// Prompt sends one prompt and waits for its turn to finish. The turn is
// found by its text, not by status alone: a notice stored while the
// session was down can wake a turn of its own when it is adopted.
func (a *archiveUnarchiveAdapter) Prompt() error {
	if _, ok := a.enabled(func(s auState) bool { return !s.archived && a.dialog == "none" }); !ok {
		return nil
	}
	name := a.next()
	text := "prompt " + name
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "answered " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, text); err != nil {
		return err
	}
	return a.waitTurnDone(a.id, text)
}

// waitTurnDone waits until the input text has been answered and the
// session is idle again with its child up.
func (a *archiveUnarchiveAdapter) waitTurnDone(id, text string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	var last string
	for {
		row, lines, err := a.s.GetSession(ctx, id)
		if err == nil {
			in, closed := -1, false
			for i, l := range lines {
				if l.Kind == "input" && strings.Contains(l.Text, text) {
					in = i
				}
				if in >= 0 && i > in && (l.Kind == "done" || l.Kind == "cancelled") {
					closed = true
				}
			}
			if in >= 0 && closed && row.Status != serve.StatusRunning && row.Live {
				return nil
			}
			last = fmt.Sprintf("status %s live %v input %v closed %v", row.Status, row.Live, in >= 0, closed)
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for %q to be answered: %s", text, last)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// Spawn starts the agent now, on a held turn.
func (a *archiveUnarchiveAdapter) Spawn() error {
	if _, ok := a.enabled(func(s auState) bool { return s.live && s.agent == "none" }); !ok {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	name := a.next()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "agent " + name + " finished"})
	id, queued, err := a.spawn(ctx, a.id, "agent "+name, 16)
	if err != nil {
		return err
	}
	if queued {
		return fmt.Errorf("agent %s queued with 16 slots", id)
	}
	a.agent, a.held = id, name
	return a.waitAgentRunning()
}

// SpawnQueued spawns the agent with the one running slot taken by a
// blocker, so it waits in the queue.
func (a *archiveUnarchiveAdapter) SpawnQueued() error {
	if _, ok := a.enabled(func(s auState) bool { return s.live && s.agent == "none" }); !ok {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if a.block == "" {
		name := a.next()
		control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "blocker " + name})
		id, _, err := a.spawn(ctx, a.filler, "blocker "+name, 16)
		if err != nil {
			return err
		}
		a.block = name
		control.WaitTaken(a.t, a.dir, name, actionTimeout)
		if _, err := waitRow(a.s, id, "the blocker running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
			return err
		}
	}
	id, queued, err := a.spawn(ctx, a.id, "queued agent", 1)
	if err != nil {
		return err
	}
	if !queued {
		return fmt.Errorf("agent %s started with the one slot taken", id)
	}
	a.agent = id
	return nil
}

func (a *archiveUnarchiveAdapter) waitAgentRunning() error {
	control.WaitTaken(a.t, a.dir, a.held, actionTimeout)
	_, err := waitRow(a.s, a.agent, "the agent running", func(r serve.Row) bool { return r.Status == serve.StatusRunning })
	return err
}

// releaseBlocker frees the slot the blocker holds and waits for it to
// be given back.
func (a *archiveUnarchiveAdapter) releaseBlocker() error {
	control.Release(a.t, a.dir, a.block)
	a.block = ""
	ctx, cancel := actionCtx()
	defer cancel()
	kids, err := a.children(ctx, a.filler)
	if err != nil {
		return err
	}
	for _, k := range kids {
		if _, err := waitRow(a.s, k.ID, "the blocker to finish", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
			return err
		}
	}
	return nil
}

// AgentStart is a slot coming free: the blocker finishes and the queue
// starts the agent, on its own turn.
func (a *archiveUnarchiveAdapter) AgentStart() error {
	if _, ok := a.enabled(func(s auState) bool { return s.agent == "queued" }); !ok {
		return nil
	}
	name := a.next()
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "agent " + name + " finished"})
	a.held = name
	if err := a.releaseBlocker(); err != nil {
		return err
	}
	return a.waitAgentRunning()
}

// AgentFinish ends the agent's turn. Its report then reaches the parent:
// a live parent wakes a turn for it (taking no queued turn: none is
// queued now), so the step waits for that to settle too.
func (a *archiveUnarchiveAdapter) AgentFinish() error {
	if _, ok := a.enabled(func(s auState) bool { return s.agent == "running" }); !ok {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if _, err := waitRow(a.s, a.agent, "the agent done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	return a.waitReported()
}

// waitReported waits for the agent's report in the parent's transcript
// and for any turn it woke to end.
func (a *archiveUnarchiveAdapter) waitReported() error {
	ctx, cancel := actionCtx()
	defer cancel()
	var row serve.Row
	var lines []serve.Line
	for {
		r, l, err := a.s.GetSession(ctx, a.id)
		if err == nil {
			row, lines = r, l
			if row.Status != serve.StatusRunning && slices.ContainsFunc(lines, func(l serve.Line) bool {
				m := agentReport.FindStringSubmatch(l.Text)
				return m != nil && m[1] == a.agent
			}) {
				return nil
			}
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for the agent %s report in the parent (status %s): %v", a.agent, row.Status, tail(lines))
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// Archive is archiveRow: the choice dialog when the row counts a
// running or queued agent, else the plain confirm.
func (a *archiveUnarchiveAdapter) Archive() error {
	st, ok := a.enabled(func(s auState) bool { return !s.archived && a.dialog == "none" })
	if !ok {
		return nil
	}
	if c := st.row.Agents; a.alwaysChoice || c != nil && c.Running+c.Queued > 0 {
		a.dialog = "choice"
	} else {
		a.dialog = "confirm"
	}
	return nil
}

func (a *archiveUnarchiveAdapter) Cancel() error {
	if a.gate.pass(a.dialog != "none") {
		a.dialog = "none"
	}
	return nil
}

func (a *archiveUnarchiveAdapter) Confirm() error {
	if !a.gate.pass(a.dialog == "confirm") {
		return nil
	}
	return a.archive(nil)
}

func (a *archiveUnarchiveAdapter) StopAndArchive() error {
	if !a.gate.pass(a.dialog == "choice") {
		return nil
	}
	if a.stopIsArchiveOnly {
		return a.archive(nil)
	}
	if err := a.archive(map[string]bool{"stopChildren": true}); err != nil {
		return err
	}
	// The stopped agent's held turn died with it; a dropped queued one
	// leaves the blocker holding a slot nothing waits for.
	a.held = ""
	if a.block != "" {
		return a.releaseBlocker()
	}
	return nil
}

func (a *archiveUnarchiveAdapter) ArchiveOnly() error {
	if !a.gate.pass(a.dialog == "choice") {
		return nil
	}
	return a.archive(nil)
}

func (a *archiveUnarchiveAdapter) archive(body any) error {
	a.dialog = "none"
	ctx, cancel := actionCtx()
	defer cancel()
	return a.post(ctx, a.id, "archive", body, nil)
}

func (a *archiveUnarchiveAdapter) Unarchive() error {
	if _, ok := a.enabled(func(s auState) bool { return s.archived }); !ok {
		return nil
	}
	ctx, cancel := actionCtx()
	defer cancel()
	return a.post(ctx, a.id, "unarchive", nil, nil)
}

// auAction counts the steps that really ran (the gate was open), so a
// green run can say how deep its walks went.
func auAction(name string, f func(*archiveUnarchiveAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*archiveUnarchiveAdapter)
		err := f(a)
		if !a.gate.off {
			if a.ran == nil {
				a.ran = map[string]int{}
			}
			a.ran[name]++
		}
		return nil, err
	}
}

var archiveUnarchiveActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Prompt":         auAction("Prompt", (*archiveUnarchiveAdapter).Prompt),
	"Spawn":          auAction("Spawn", (*archiveUnarchiveAdapter).Spawn),
	"SpawnQueued":    auAction("SpawnQueued", (*archiveUnarchiveAdapter).SpawnQueued),
	"AgentStart":     auAction("AgentStart", (*archiveUnarchiveAdapter).AgentStart),
	"AgentFinish":    auAction("AgentFinish", (*archiveUnarchiveAdapter).AgentFinish),
	"Archive":        auAction("Archive", (*archiveUnarchiveAdapter).Archive),
	"Cancel":         auAction("Cancel", (*archiveUnarchiveAdapter).Cancel),
	"Confirm":        auAction("Confirm", (*archiveUnarchiveAdapter).Confirm),
	"StopAndArchive": auAction("StopAndArchive", (*archiveUnarchiveAdapter).StopAndArchive),
	"ArchiveOnly":    auAction("ArchiveOnly", (*archiveUnarchiveAdapter).ArchiveOnly),
	"Unarchive":      auAction("Unarchive", (*archiveUnarchiveAdapter).Unarchive),
}}

// As the example: a walk is checked only up to its first disabled
// action, so longer walks buy little; TestArchiveUnarchivePaths is what
// reaches every transition.
func archiveUnarchiveOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

// archiveUnarchiveHistory reads what a parent's transcript can show.
// Archiving writes nothing to history, so every input is a Prompt,
// except the turn an agent's report wakes in a live parent: that input,
// or the notice stored in a parent that was down, is the agent having
// been spawned and run to its end (once per agent: a stored notice is
// delivered again when the parent comes back).
func archiveUnarchiveHistory(entries []history.Entry) []tracecheck.Step {
	hist := func(b bool) map[string]any { return map[string]any{"Session#0.history": b} }
	steps := []tracecheck.Step{{Action: "Init", State: hist(false)}}
	seen := map[string]bool{}
	for _, e := range entries {
		if e.Kind != "input" && e.Kind != "notice" {
			continue
		}
		text, _ := e.Data["text"].(string)
		if m := agentReport.FindStringSubmatch(text); m != nil {
			if m[2] == "finished" && !seen[m[1]] {
				seen[m[1]] = true
				steps = append(steps,
					tracecheck.Step{Action: "Session#0.Spawn"},
					tracecheck.Step{Action: "Session#0.AgentFinish", State: map[string]any{"Session#0.agent": "done"}})
			}
			continue
		}
		if e.Kind == "input" {
			steps = append(steps, tracecheck.Step{Action: "Session#0.Prompt", State: hist(true)})
		}
	}
	return steps
}

// agentReport is serve's reportText header: [agent <title> · <id> <word>].
var agentReport = regexp.MustCompile(`\[agent [^\]]* · (\S+) (finished|stopped|failed)\]`)

func init() { historyProjections["archive_unarchive"] = archiveUnarchiveHistory }

func TestArchiveUnarchive(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newArchiveUnarchiveAdapter(t)
	if err := runMBT(t, "archive_unarchive", a, archiveUnarchiveActions, archiveUnarchiveOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("steps run: %v", a.ran)
	g, err := tracecheck.Load(fizzCheck(t, "archive_unarchive"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), archiveUnarchiveHistory)
	}
}

// An Archive that offers the choice dialog with no agent to stop opens
// "choice" where the spec says "confirm"; the run must catch it. (A
// deeper bug, such as Stop and archive leaving the agent running, is
// out of the random walks' reach: see TestArchiveUnarchivePaths.)
func TestArchiveUnarchiveCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newArchiveUnarchiveAdapter(t)
	a.alwaysChoice = true
	if err := runMBT(t, "archive_unarchive", a, archiveUnarchiveActions, archiveUnarchiveOptions()); err == nil {
		t.Fatal("a run whose Archive always asks about agents passed; the runner is not checking state")
	}
}

// The random walks above rarely get deep: the runner picks among all
// eleven actions, disabled ones included, and a walk's check ends at the
// first disabled one, so Prompt, Spawn, Archive, StopAndArchive comes up
// about once in ten thousand walks. This walks every generated path in
// testdata/archive_unarchive/paths.json instead (together they cover
// every state and transition of the spec) on the same adapter,
// comparing its state with the path's after every step.
func TestArchiveUnarchivePaths(t *testing.T) {
	t.Parallel()
	a := newArchiveUnarchiveAdapter(t)
	if err := walkPaths(a); err != nil {
		t.Fatal(err)
	}
	g, err := tracecheck.Load(filepath.Join("..", "testdata", "archive_unarchive"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), archiveUnarchiveHistory)
	}
}

// The paths walk must fail on the wiring bug the random run is tested
// with, or its green proves nothing either.
func TestArchiveUnarchivePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newArchiveUnarchiveAdapter(t)
	a.stopIsArchiveOnly = true
	if err := walkPaths(a); err == nil {
		t.Fatal("a paths walk whose Stop and archive leaves the agent running passed")
	}
}

func walkPaths(a *archiveUnarchiveAdapter) error {
	b, err := os.ReadFile(filepath.Join("..", "testdata", "archive_unarchive", "paths.json"))
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
	acts := archiveUnarchiveActions["Session"]
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d: Init: %w", pi, err)
		}
		var names []string
		for si, step := range p.Trace {
			name := strings.TrimPrefix(step.Action, "Session#0.")
			names = append(names, name)
			if si > 0 {
				if _, err := acts[name](a, nil); err != nil {
					return fmt.Errorf("path %d %v: %w", pi, names, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d %v: the adapter found %s not enabled", pi, names, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d %v: %w", pi, names, err)
			}
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Session#0.")
				if ok && got[field] != v {
					return fmt.Errorf("path %d %v: %s is %v, the spec says %v", pi, names, field, got[field], v)
				}
			}
		}
		if err := a.Cleanup(); err != nil {
			return fmt.Errorf("path %d %v: Cleanup: %w", pi, names, err)
		}
	}
	return nil
}

// tail is the last few transcript lines, for an error.
func tail(lines []serve.Line) []string {
	var out []string
	for _, l := range lines[max(0, len(lines)-6):] {
		out = append(out, l.Kind+": "+l.Text)
	}
	return out
}
