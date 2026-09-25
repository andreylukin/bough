//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/changes_checkpoint_edge.fizz against a real serve: one session
// per walk in a fresh checkout, its turns real turns through llm-control
// (turn k writes file "a" or "b" with the write tool), /undo the real
// command, and the page's reads serve's GET changes, edits, edits?turn=
// and diff. The page around the reads (open, scope, behind, which read is
// in flight) is the adapter's, as the spec has it; what it shows (view,
// listed, diff) is what serve answered.
//
// The faults. RemoveWorktree removes the checkout. The other three go
// through a git on serve's PATH that is the real one except, for git run
// by serve itself (its parent is `bough serve --run`) while a flag file
// exists: slow sleeps past changesTimeout before running git, snapfail
// fails write-tree (the snapshot of now), and cpgone fails any command
// naming an object id, as git does when the checkpoint objects are gone.
// The session's own git (its checkpoints, /undo) is left alone: the spec
// breaks only what the page reads.
type cceAdapter struct {
	t     *testing.T
	s     *servetest.Server
	dir   string // llm-control's queue
	flags string // the fake git's flag files
	gate  gate
	n     int

	repo, id string
	ids      []string
	seq      [2]int64  // turn k's input seq
	name     [2]string // turn k's queued names (+"a" the held start, +"b" the held end)
	queued   []string  // queued and not yet taken, removed at Init

	// The world as the adapter drove it (read back from the transcript
	// in GetState).
	t12   [2]string
	fault string

	// The page.
	open         bool
	scope, view  string
	listed       []string
	rfault, diff string
	behind       bool

	taken map[string]int

	// snapNothing is TestChangesCheckpointEdgeCatchesWrongAdapter's bug:
	// SnapshotFails breaks nothing.
	snapNothing bool
}

func newCCEAdapter(t *testing.T) *cceAdapter {
	real, err := exec.LookPath("git")
	if err != nil {
		t.Skip("no git")
	}
	bin, flags := t.TempDir(), t.TempDir()
	script := `#!/bin/bash
f='` + flags + `'
if [ -e "$f/slow" ] || [ -e "$f/snapfail" ] || [ -e "$f/cpgone" ]; then
  case "$(ps -o command= -p $PPID)" in
  *" serve --run "*)
    # Past changesTimeout, then git: a read bounded by it is cut short,
    # one that is not answers late. The sleep holds none of git's pipes.
    [ -e "$f/slow" ] && sleep 6 </dev/null >/dev/null 2>&1
    if [ -e "$f/snapfail" ]; then
      for a in "$@"; do
        [ "$a" = write-tree ] && { echo "fatal: unable to write new index file" >&2; exit 128; }
      done
    fi
    if [ -e "$f/cpgone" ]; then
      for a in "$@"; do
        [[ "$a" =~ ^[0-9a-f]{40} ]] && { echo "fatal: bad object ${a:0:40}" >&2; exit 128; }
      done
    fi
    ;;
  esac
fi
exec '` + real + `' "$@"
`
	if err := os.WriteFile(filepath.Join(bin, "git"), []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{"PATH=" + bin + string(os.PathListSeparator) + os.Getenv("PATH")}})
	return &cceAdapter{t: t, s: s, dir: control.Dir(s.Home), flags: flags, taken: map[string]int{}}
}

// git runs git in dir with a HOME of the serve's, never the user's.
func (a *cceAdapter) git(dir string, args ...string) error {
	cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.name=model", "-c", "user.email=model@test", "-c", "commit.gpgsign=false"}, args...)...)
	cmd.Env = append(os.Environ(), "HOME="+a.s.Home, "GIT_CONFIG_NOSYSTEM=1")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git %v: %v\n%s", args, err, out)
	}
	return nil
}

func (a *cceAdapter) Init() error {
	a.Cleanup()
	a.n++
	for _, f := range []string{"slow", "snapfail", "cpgone"} {
		os.Remove(filepath.Join(a.flags, f))
	}
	// The last walk's session is done with: stop its child.
	if a.id != "" {
		ctx, cancel := actionCtx()
		a.s.Archive(ctx, a.id)
		cancel()
	}
	a.repo = filepath.Join(a.s.Root, fmt.Sprintf("w%04d", a.n))
	if err := os.MkdirAll(a.repo, 0o755); err != nil {
		return err
	}
	for _, step := range []func() error{
		func() error { return a.git(a.repo, "init", "-q", "-b", "main") },
		func() error { return os.WriteFile(filepath.Join(a.repo, "README"), []byte("base\n"), 0o644) },
		func() error { return a.git(a.repo, "add", "README") },
		func() error { return a.git(a.repo, "commit", "-q", "-m", "base") },
	} {
		if err := step(); err != nil {
			return err
		}
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.repo, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.seq, a.name = [2]int64{}, [2]string{}
	a.t12, a.fault = [2]string{"none", "none"}, "none"
	a.open, a.scope, a.view, a.listed = false, "session", "none", []string{}
	a.rfault, a.diff, a.behind = "none", "none", false
	a.gate.reset()
	return nil
}

// Cleanup drops what the walk queued and never used, so the next walk's
// session takes only its own turns. A turn left held stays held until
// Init archives its session: released, it would end without the write
// the spec says comes first, and its transcript would fail the trace
// check for the adapter's sake.
func (a *cceAdapter) Cleanup() error {
	for _, q := range a.queued {
		os.Remove(filepath.Join(a.dir, q+".json"))
	}
	a.queued = nil
	return nil
}

func (a *cceAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// GetState reads t1 and t2 off the session's transcript; the page's
// fields are what its last reads answered.
func (a *cceAdapter) GetState() (map[string]any, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	t := cceTurns(lines)
	return map[string]any{
		"t1": t[0], "t2": t[1], "fault": a.fault,
		"open": a.open, "scope": a.scope, "view": a.view, "listed": slices.Clone(a.listed),
		"rfault": a.rfault, "behind": a.behind, "diff": a.diff,
	}, nil
}

// cceTurns is each turn's status as the transcript has it: its input
// (a typed line, not a /command), its write's call ending, its done,
// an undo naming its seq.
func cceTurns(lines []serve.Line) [2]string {
	st := [2]string{"none", "none"}
	seqs := [2]int64{}
	k := -1
	for _, l := range lines {
		switch {
		case l.Kind == "input" && !strings.HasPrefix(l.Text, "/"):
			k++
			if k < 2 {
				st[k], seqs[k] = "running", l.Seq
			}
		case k < 0 || k > 1:
		case cceWrote(l) && st[k] == "running":
			st[k] = "wrote"
		case l.Kind == "done" && st[k] != "done":
			st[k] = "done"
		}
		if l.Kind == "undo" {
			if s, ok := l.Data["seq_of_turn"].(float64); ok {
				for i := range 2 {
					if seqs[i] == int64(s) && seqs[i] != 0 {
						st[i] = "undone"
					}
				}
			}
		}
	}
	return st
}

// cceWrote is the write call's recorded end (its live start is not
// recorded and carries phase "start").
func cceWrote(l serve.Line) bool {
	return l.Kind == "call" && l.Data["tool"] == "write" && l.Data["phase"] != "start"
}

// --- the page

func (a *cceAdapter) running() bool {
	return slices.Contains([]string{"running", "wrote"}, a.t12[0]) || slices.Contains([]string{"running", "wrote"}, a.t12[1])
}

func (a *cceAdapter) settled() bool {
	return !a.open || (a.view != "loading" && !a.behind && a.diff != "loading")
}

func (a *cceAdapter) tick() {
	if a.open {
		a.behind = true
	}
}

// get is one GET against serve: the status and the body.
func (a *cceAdapter) get(path string) (int, []byte, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+path, nil)
	if err != nil {
		return 0, nil, err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	return resp.StatusCode, b, err
}

func (a *cceAdapter) turnOf() int {
	if a.scope == "turn1" {
		return 0
	}
	return 1
}

// read is the page's list read for its scope: an answer that is not 200
// is the page's error, repo=false its "not a repository", undone its
// "this turn was undone".
func (a *cceAdapter) read() (string, []string, error) {
	sp := "/api/sessions/" + url.PathEscape(a.id)
	path := sp + "/edits"
	switch a.scope {
	case "tree":
		path = sp + "/changes"
	case "turn1", "turn2":
		path = fmt.Sprintf("%s/edits?turn=%d", sp, a.seq[a.turnOf()])
	}
	code, b, err := a.get(path)
	if err != nil {
		return "", nil, err
	}
	if code != http.StatusOK {
		return "error", []string{}, nil
	}
	var r struct {
		Repo   bool
		Undone bool
		Files  []struct{ Path string }
	}
	if err := json.Unmarshal(b, &r); err != nil {
		return "", nil, fmt.Errorf("GET %s: %w: %s", path, err, b)
	}
	fs := []string{}
	for _, f := range r.Files {
		fs = append(fs, f.Path)
	}
	slices.Sort(fs)
	switch {
	case r.Undone:
		return "undone", fs, nil
	case !r.Repo:
		return "not_repo", fs, nil
	case len(fs) == 0:
		return "no_changes", fs, nil
	}
	return "files", fs, nil
}

func (a *cceAdapter) reread() error {
	v, fs, err := a.read()
	if err != nil {
		return err
	}
	a.view, a.listed, a.rfault, a.behind, a.diff = v, fs, a.fault, false, "none"
	return nil
}

func (a *cceAdapter) opening(scope string) {
	a.open, a.scope, a.view, a.listed, a.rfault, a.behind, a.diff = true, scope, "loading", []string{}, "none", false, "none"
}

func (a *cceAdapter) OpenSession() error {
	if a.gate.pass(!a.open) {
		a.opening("session")
	}
	return nil
}

func (a *cceAdapter) OpenTurn1() error {
	if a.gate.pass(!a.open && (a.t12[0] == "done" || a.t12[0] == "undone")) {
		a.opening("turn1")
	}
	return nil
}

func (a *cceAdapter) OpenTurn2() error {
	if a.gate.pass(!a.open && (a.t12[1] == "done" || a.t12[1] == "undone")) {
		a.opening("turn2")
	}
	return nil
}

func (a *cceAdapter) OpenTree() error {
	if a.gate.pass(!a.open) {
		a.opening("tree")
	}
	return nil
}

func (a *cceAdapter) Close() error {
	if a.gate.pass(a.open && a.settled()) {
		a.open, a.scope, a.view, a.listed, a.rfault, a.diff = false, "session", "none", []string{}, "none", "none"
	}
	return nil
}

func (a *cceAdapter) DiffFile() error {
	if a.gate.pass(a.open && a.view == "files" && !a.behind && a.diff == "none") {
		a.diff = "loading"
	}
	return nil
}

func (a *cceAdapter) Answer() error {
	if !a.gate.pass(a.open && a.view == "loading") {
		return nil
	}
	return a.reread()
}

func (a *cceAdapter) Poll() error {
	if !a.gate.pass(a.open && a.view != "loading" && a.behind && a.diff != "loading") {
		return nil
	}
	return a.reread()
}

// DiffAnswer is the open card's GET diff: 504 is the page's "took too
// long", any other failure its "couldn't read the diff".
func (a *cceAdapter) DiffAnswer() error {
	if !a.gate.pass(a.diff == "loading") {
		return nil
	}
	q := url.Values{"path": {a.listed[0]}}
	switch a.scope {
	case "session", "tree":
		q.Set("scope", a.scope)
	default:
		q.Set("scope", "turn")
		q.Set("turn", fmt.Sprint(a.seq[a.turnOf()]))
	}
	code, b, err := a.get("/api/sessions/" + url.PathEscape(a.id) + "/diff?" + q.Encode())
	if err != nil {
		return err
	}
	switch code {
	case http.StatusOK:
		var r struct{ Diff string }
		if err := json.Unmarshal(b, &r); err != nil {
			return err
		}
		a.diff = "shown"
		if r.Diff == "" {
			a.diff = "empty"
		}
	case http.StatusGatewayTimeout:
		a.diff = "timeout"
	default:
		a.diff = "error"
	}
	return nil
}

// --- the session's world

func (a *cceAdapter) waitFile(name string) error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(filepath.Join(a.dir, name)); err == nil {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("llm-control: no %s after %s", name, actionTimeout)
}

// waitTurns waits until the transcript has turn k at want.
func (a *cceAdapter) waitTurns(k int, want string) error {
	ctx, cancel := actionCtx()
	defer cancel()
	for {
		_, lines, err := a.s.GetSession(ctx, a.id)
		if err == nil && cceTurns(lines)[k] == want {
			return nil
		}
		select {
		case <-ctx.Done():
			var kinds []string
			for _, l := range lines {
				kinds = append(kinds, l.Kind+":"+l.Text)
			}
			return fmt.Errorf("waiting for turn %d %s: %w; transcript %q", k+1, want, ctx.Err(), kinds)
		case <-time.After(30 * time.Millisecond):
		}
	}
}

func (a *cceAdapter) StartTurn() error {
	if !a.gate.pass(a.settled() && !a.running() && (a.t12[0] == "none" || a.t12[1] == "none")) {
		return nil
	}
	k := 0
	if a.t12[0] != "none" {
		k = 1
	}
	a.name[k] = fmt.Sprintf("w%05dt%d", a.n, k+1)
	control.Queue(a.t, a.dir, a.name[k]+"a", control.Turn{Mode: "block", Text: "unused"})
	control.Queue(a.t, a.dir, a.name[k]+"b", control.Turn{Mode: "block", Text: fmt.Sprintf("turn %d done", k+1)})
	a.queued = append(a.queued, a.name[k]+"a", a.name[k]+"b")
	a.t12[k] = "running"
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, fmt.Sprintf("turn %d of walk %d", k+1, a.n)); err != nil {
		return err
	}
	if err := a.waitFile(a.name[k] + "a.taken"); err != nil {
		return err
	}
	if err := a.waitTurns(k, "running"); err != nil {
		return err
	}
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	n := -1
	for _, l := range lines {
		if l.Kind == "input" && !strings.HasPrefix(l.Text, "/") {
			if n++; n == k {
				a.seq[k] = l.Seq
			}
		}
	}
	a.tick()
	return nil
}

func (a *cceAdapter) cur(st string) int {
	if a.t12[0] == st {
		return 0
	}
	return 1
}

// AgentWrite answers the held request with a write of the turn's file;
// the engine runs it and asks again, which takes the turn's end.
func (a *cceAdapter) AgentWrite() error {
	if !a.gate.pass(a.settled() && (a.t12[0] == "running" || a.t12[1] == "running")) {
		return nil
	}
	k := a.cur("running")
	f := string(rune('a' + k))
	control.ReleaseWith(a.t, a.dir, a.name[k]+"a", control.Turn{Call: &control.Call{Name: "write", Args: map[string]any{"path": f, "content": f + " from turn " + fmt.Sprint(k+1) + "\n"}}})
	a.t12[k] = "wrote"
	if err := a.waitFile(a.name[k] + "b.taken"); err != nil {
		return err
	}
	if err := a.waitTurns(k, "wrote"); err != nil {
		return err
	}
	a.tick()
	return nil
}

func (a *cceAdapter) TurnEnds() error {
	if !a.gate.pass(a.settled() && (a.t12[0] == "wrote" || a.t12[1] == "wrote")) {
		return nil
	}
	k := a.cur("wrote")
	control.Release(a.t, a.dir, a.name[k]+"b")
	a.t12[k] = "done"
	a.queued = slices.DeleteFunc(a.queued, func(q string) bool { return strings.HasPrefix(q, a.name[k]) })
	if err := a.waitTurns(k, "done"); err != nil {
		return err
	}
	if _, err := waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning }); err != nil {
		return err
	}
	a.tick()
	return nil
}

// Undo is /undo: the last done turn not undone.
func (a *cceAdapter) Undo() error {
	if !a.gate.pass(a.settled() && !a.running() && (a.t12[0] == "done" || a.t12[1] == "done") && a.fault != "cwd_gone") {
		return nil
	}
	k := 0
	if a.t12[1] == "done" {
		k = 1
	}
	a.t12[k] = "undone"
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "/undo"); err != nil {
		return err
	}
	if err := a.waitTurns(k, "undone"); err != nil {
		return err
	}
	a.tick()
	return nil
}

func (a *cceAdapter) fail(f string) error {
	a.fault = f
	if a.open && a.scope == "tree" {
		a.behind = true
	}
	switch f {
	case "cwd_gone":
		return os.RemoveAll(a.repo)
	case "snap_fails":
		if a.snapNothing {
			return nil
		}
		return os.WriteFile(filepath.Join(a.flags, "snapfail"), nil, 0o644)
	case "cp_gone":
		return os.WriteFile(filepath.Join(a.flags, "cpgone"), nil, 0o644)
	}
	return os.WriteFile(filepath.Join(a.flags, "slow"), nil, 0o644)
}

func (a *cceAdapter) faultable() bool {
	return a.settled() && !a.running() && a.fault == "none" && a.diff == "none"
}

func (a *cceAdapter) CorruptCheckpoint() error {
	if !a.gate.pass(a.faultable() && a.t12[0] != "none") {
		return nil
	}
	return a.fail("cp_gone")
}

func (a *cceAdapter) SnapshotFails() error {
	if !a.gate.pass(a.faultable() && a.t12[0] != "none") {
		return nil
	}
	return a.fail("snap_fails")
}

func (a *cceAdapter) TreeGrows() error {
	if !a.gate.pass(a.faultable()) {
		return nil
	}
	return a.fail("slow")
}

func (a *cceAdapter) RemoveWorktree() error {
	if !a.gate.pass(a.faultable()) {
		return nil
	}
	return a.fail("cwd_gone")
}

// cceAction counts the actions a walk took, so a run that never reached
// one says so.
func cceAction(name string, f func(*cceAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*cceAdapter)
		open := !a.gate.off
		err := f(a)
		if open && !a.gate.off {
			a.taken[name]++
		}
		return nil, err
	}
}

var cceActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"OpenSession":       cceAction("OpenSession", (*cceAdapter).OpenSession),
	"OpenTurn1":         cceAction("OpenTurn1", (*cceAdapter).OpenTurn1),
	"OpenTurn2":         cceAction("OpenTurn2", (*cceAdapter).OpenTurn2),
	"OpenTree":          cceAction("OpenTree", (*cceAdapter).OpenTree),
	"Close":             cceAction("Close", (*cceAdapter).Close),
	"DiffFile":          cceAction("DiffFile", (*cceAdapter).DiffFile),
	"Answer":            cceAction("Answer", (*cceAdapter).Answer),
	"Poll":              cceAction("Poll", (*cceAdapter).Poll),
	"DiffAnswer":        cceAction("DiffAnswer", (*cceAdapter).DiffAnswer),
	"StartTurn":         cceAction("StartTurn", (*cceAdapter).StartTurn),
	"AgentWrite":        cceAction("AgentWrite", (*cceAdapter).AgentWrite),
	"TurnEnds":          cceAction("TurnEnds", (*cceAdapter).TurnEnds),
	"Undo":              cceAction("Undo", (*cceAdapter).Undo),
	"CorruptCheckpoint": cceAction("CorruptCheckpoint", (*cceAdapter).CorruptCheckpoint),
	"SnapshotFails":     cceAction("SnapshotFails", (*cceAdapter).SnapshotFails),
	"TreeGrows":         cceAction("TreeGrows", (*cceAdapter).TreeGrows),
	"RemoveWorktree":    cceAction("RemoveWorktree", (*cceAdapter).RemoveWorktree),
}}

// Seventeen actions and few enabled at once, so most random walks stop
// within a step or two (150 walks took 5 turns): many cheap walks.
func cceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 600, "max-actions": 12, "max-parallel-runs": 0}
}

// cceHistory reads a transcript as the spec's world steps, the page
// closed throughout: a typed input is StartTurn, its write call's end
// AgentWrite, its done TurnEnds, an undo entry Undo.
func cceHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Page#0.t1": "none", "Page#0.t2": "none"}}}
	seqs := map[int64]int{}
	k := 0
	st := func(n int, v string) map[string]any { return map[string]any{fmt.Sprintf("Page#0.t%d", n): v} }
	for _, e := range entries {
		switch {
		case e.Kind == "input" && !strings.HasPrefix(history.Prompt(e), "/"):
			k++
			seqs[e.Seq] = k
			steps = append(steps, tracecheck.Step{Action: "Page#0.StartTurn", State: st(k, "running")})
		case e.Kind == "call" && e.Data["tool"] == "write" && e.Data["phase"] != "start":
			steps = append(steps, tracecheck.Step{Action: "Page#0.AgentWrite", State: st(k, "wrote")})
		case e.Kind == "done":
			steps = append(steps, tracecheck.Step{Action: "Page#0.TurnEnds", State: st(k, "done")})
		case e.Kind == "undo":
			s, _ := e.Data["seq_of_turn"].(float64)
			steps = append(steps, tracecheck.Step{Action: "Page#0.Undo", State: st(seqs[int64(s)], "undone")})
		}
	}
	return steps
}

func init() { historyProjections["changes_checkpoint_edge"] = cceHistory }

// walkCCEPaths walks every generated path and compares the state after
// each step, path i on adapter i mod len(as), each adapter its own serve
// walking its share in turn: at a few seconds a path (a slow read is 5 s
// of real time) one serve took over half an hour. stopFirst ends an
// adapter's share at its first divergence.
func walkCCEPaths(t *testing.T, as []*cceAdapter, cover tracecheck.Cover, stopFirst bool) error {
	t.Helper()
	b, err := pathsJSONCover("changes_checkpoint_edge", cover)
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
	var (
		mu     sync.Mutex
		errs   []error
		walked int
		wg     sync.WaitGroup
	)
	start := time.Now()
	for n, a := range as {
		wg.Go(func() {
			for i := n; i < len(doc.Paths); i += len(as) {
				err := a.walk(doc.Paths[i].Trace)
				mu.Lock()
				walked++
				if err != nil {
					errs = append(errs, fmt.Errorf("path %d: %w", i, err))
					t.Logf("path %d: %v", i, err)
				}
				if walked%20 == 0 {
					t.Logf("%d/%d paths walked in %s, %d failed", walked, len(doc.Paths), time.Since(start).Round(time.Second), len(errs))
				}
				mu.Unlock()
				if err != nil && stopFirst {
					break
				}
			}
			a.Cleanup()
		})
	}
	wg.Wait()
	taken := map[string]int{}
	for _, a := range as {
		for k, v := range a.taken {
			taken[k] += v
		}
	}
	t.Logf("%d paths on %d serves in %s, %d failed, actions taken: %v", len(doc.Paths), len(as), time.Since(start).Round(time.Second), len(errs), taken)
	return errors.Join(errs...)
}

func (a *cceAdapter) walk(trace []tracecheck.Step) error {
	var did []string
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "Page#0.")
		did = append(did, name)
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = cceActions["Page"][name](a, nil)
			if err == nil && a.gate.off {
				err = errors.New("the adapter found it disabled")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w; walked %v", j, name, err, did)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w; walked %v", j, name, err, did)
		}
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Page#0.")
			if ok && fmt.Sprint(got[f]) != fmt.Sprint(v) {
				diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s): %s; walked %v", j, name, strings.Join(diff, "; "), did)
		}
	}
	return nil
}

func checkCCEHistories(t *testing.T, as ...*cceAdapter) {
	t.Helper()
	g, err := tracecheck.Load(fizzCheck(t, "changes_checkpoint_edge"))
	if err != nil {
		t.Fatal(err)
	}
	sessions, turned := 0, 0
	for _, a := range as {
		for _, id := range a.ids {
			entries := sessionHistory(t, a.s.Home, id)
			checkHistory(t, g, entries, cceHistory)
			sessions++
			if len(cceHistory(entries)) > 1 {
				turned++
			}
		}
	}
	if turned == 0 {
		t.Errorf("no session of %d took a turn; the trace check checked nothing", sessions)
	}
	t.Logf("trace-checked %d sessions' histories (%d with turns)", sessions, turned)
}

// TestChangesCheckpointEdge lets fizzbee-mbt walk the spec at random
// (the exhaustive run only; see runMBT).
func TestChangesCheckpointEdge(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCCEAdapter(t)
	if err := runMBT(t, "changes_checkpoint_edge", a, cceActions, cceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	a.Cleanup()
	t.Logf("actions taken: %v", a.taken)
	checkCCEHistories(t, a)
}

// TestChangesCheckpointEdgePaths walks the generated paths against three
// serves.
func TestChangesCheckpointEdgePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	as := []*cceAdapter{newCCEAdapter(t), newCCEAdapter(t), newCCEAdapter(t)}
	if err := walkCCEPaths(t, as, envCover(), false); err != nil {
		t.Fatalf("spec path: %v", err)
	}
	checkCCEHistories(t, as...)
}

// The projection must refuse a transcript the model forbids: a turn
// that ends before it wrote, a second undo of one turn.
func TestChangesCheckpointEdgeHistoryProjection(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	g, err := tracecheck.Load(fizzCheck(t, "changes_checkpoint_edge"))
	if err != nil {
		t.Fatal(err)
	}
	in := func(seq int64) history.Entry {
		return history.Entry{Seq: seq, Kind: "input", Data: map[string]any{"text": "turn"}}
	}
	wrote := history.Entry{Kind: "call", Data: map[string]any{"tool": "write", "id": "c1"}}
	done := history.Entry{Kind: "done"}
	undo := func(seq int64) history.Entry {
		return history.Entry{Kind: "undo", Data: map[string]any{"seq_of_turn": float64(seq)}}
	}
	ok := []history.Entry{in(1), wrote, done, in(5), wrote, done, undo(5), undo(1)}
	if v := g.Check(cceHistory(ok)); v != nil {
		t.Fatalf("two turns, both undone: %v", v)
	}
	for name, es := range map[string][]history.Entry{
		"a turn that ends before it wrote": {in(1), done},
		"one turn undone twice":            {in(1), wrote, done, undo(1), undo(1)},
	} {
		if v := g.Check(cceHistory(es)); v == nil {
			t.Errorf("%s: accepted as a path in the model", name)
		} else {
			t.Logf("%s: refused as expected: %v", name, v)
		}
	}
}

// A run whose SnapshotFails breaks nothing must fail, or a green
// TestChangesCheckpointEdgePaths proves nothing.
func TestChangesCheckpointEdgeCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCCEAdapter(t)
	a.snapNothing = true
	err := walkCCEPaths(t, []*cceAdapter{a}, tracecheck.CoverStates, true)
	if err == nil {
		t.Fatal("a run whose SnapshotFails breaks nothing passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
