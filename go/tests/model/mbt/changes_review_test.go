//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/changes_review.fizz against a real serve: the Changes page's
// reads (GET edits, GET edits?turn=, GET changes, GET diff) are the
// server's, and the page around them (which tab, which cards are open,
// loading/failed/stale) is the adapter's, following changes.tsx the way
// the spec does. What the server is checked for is the data: that a
// shell edit is in the session's edits, that the tree is git's, that a
// session with no checkpoint has no session diff, that a hand edit is
// in the next tree read.
//
// The three sessions:
//
//	A  a checkout, fresh per walk (made on first use, so a walk that
//	   never touches it costs nothing). "h" is tracked and dirty from
//	   the start; AgentEdit is a real turn whose bash call writes "a".
//	N  a checkout whose one turn wrote "n" with the write tool while
//	   .git/objects was read-only, so the turn's input has no
//	   checkpoint (git add -A could not hash the dirty "n"). Made once.
//	R  a plain directory. Made once.
//
// Failures (AnswerFails, PollFails) are the page's fetch failing: the
// server has no seam that fails a read short of git taking 5 s, so the
// adapter takes the failed branch without a read, as the page would.
type changesAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate
	n    int // walks and turns, for unique names

	nID, rID string

	// A's world, fresh per walk.
	aDir, aID string
	edited    bool
	turnSeq   int64
	aIDs      []string

	// The page.
	open        bool
	sid         string
	turn        bool
	scope       string
	edits, tree string // none | loading | ok | failed | stale
	sess, turnL []serve.Edit
	treeL       []serve.Change
	ca, ch, cn  bool
	diff        []string

	// outOfBand is the deliberate bug TestChangesReviewCatchesWrongAdapter
	// injects: AgentEdit's turn only answers, and the adapter writes "a"
	// itself while it is held, so no call of the session's made the edit.
	outOfBand bool
}

func newChangesAdapter(t *testing.T) *changesAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &changesAdapter{t: t, s: s, dir: control.Dir(s.Home)}
	a.setupN()
	r, err := s.CreateSession(t.Context(), s.Dir(t, "plain"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.rID = r.ID
	return a
}

// git runs git in dir with a HOME of the serve's, never the user's.
func (a *changesAdapter) git(dir string, args ...string) error {
	cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.name=model", "-c", "user.email=model@test", "-c", "commit.gpgsign=false"}, args...)...)
	cmd.Env = append(os.Environ(), "HOME="+a.s.Home, "GIT_CONFIG_NOSYSTEM=1")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git %v: %v\n%s", args, err, out)
	}
	return nil
}

// checkout makes a repository under the serve's root with f committed
// and then changed, so f is dirty (tracked, modified) from the start.
func (a *changesAdapter) checkout(name, f string) (string, error) {
	dir := a.s.Dir(a.t, name)
	p := filepath.Join(dir, f)
	for _, step := range []func() error{
		func() error { return a.git(dir, "init", "-q", "-b", "main") },
		func() error { return os.WriteFile(p, []byte("one\n"), 0o644) },
		func() error { return a.git(dir, "add", f) },
		func() error { return a.git(dir, "commit", "-q", "-m", "base") },
		func() error { return os.WriteFile(p, []byte("one\ntwo\n"), 0o644) },
	} {
		if err := step(); err != nil {
			return "", err
		}
	}
	return dir, nil
}

// setupN makes N: its one turn runs while .git/objects cannot be
// written, so the checkpoint before it fails and only the write tool's
// tally records "n".
func (a *changesAdapter) setupN() {
	t := a.t
	dir, err := a.checkout("nocp", "n")
	if err != nil {
		t.Fatal(err)
	}
	objects := filepath.Join(dir, ".git", "objects")
	chmodTree := func(mode os.FileMode) {
		filepath.Walk(objects, func(p string, fi os.FileInfo, err error) error {
			if err == nil && fi.IsDir() {
				os.Chmod(p, mode)
			}
			return nil
		})
	}
	chmodTree(0o555)
	// Put back before the root is removed, whatever happens here.
	t.Cleanup(func() { chmodTree(0o755) })
	row, err := a.s.CreateSession(t.Context(), dir, "")
	if err != nil {
		t.Fatal(err)
	}
	a.nID = row.ID
	control.Queue(t, a.dir, "000n1", control.Turn{Mode: "ok", Calls: []control.Call{{Name: "write", Args: map[string]any{"path": "n", "content": "one\ntwo\nthree\n"}}}})
	control.Queue(t, a.dir, "000n2", control.Turn{Mode: "ok", Text: "wrote n"})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.nID, "write n"); err != nil {
		t.Fatal(err)
	}
	lines, err := a.waitDone(a.nID, 1)
	if err != nil {
		t.Fatal(err)
	}
	chmodTree(0o755)
	for _, l := range lines {
		if l.Kind == "input" && l.Data["checkpoint"] != nil {
			t.Fatalf("N's turn took a checkpoint %v; the no-checkpoint case is not set up", l.Data["checkpoint"])
		}
	}
}

// waitDone waits for the session's nth done entry and its row to leave
// running, and returns the transcript.
func (a *changesAdapter) waitDone(id string, n int) ([]serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	for {
		row, lines, err := a.s.GetSession(ctx, id)
		if err == nil && row.Status != serve.StatusRunning {
			done := 0
			for _, l := range lines {
				if l.Kind == "done" {
					done++
				}
			}
			if done >= n {
				return lines, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("waiting for done #%d of %s: %w (last status %q, err %v)", n, id, ctx.Err(), row.Status, err)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// ensureA makes this walk's A on first use.
func (a *changesAdapter) ensureA() error {
	if a.aID != "" {
		return nil
	}
	dir, err := a.checkout(fmt.Sprintf("a%04d", a.n), "h")
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, dir, "")
	if err != nil {
		return err
	}
	a.aDir, a.aID = dir, row.ID
	a.aIDs = append(a.aIDs, row.ID)
	return nil
}

func (a *changesAdapter) Init() error {
	a.n++
	a.aDir, a.aID, a.edited, a.turnSeq = "", "", false, 0
	a.sid = "A"
	a.reset()
	a.gate.reset()
	return nil
}

func (a *changesAdapter) reset() {
	a.open, a.turn, a.scope = false, false, "session"
	a.edits, a.tree = "none", "none"
	a.sess, a.turnL, a.treeL = nil, nil, nil
	a.ca, a.ch, a.cn = false, false, false
	a.diff = []string{}
}

func (a *changesAdapter) Cleanup() error { return nil }

func (a *changesAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// GetState reads eseen and tagent off the page's last reads (the
// server's lists), and behind off the server now: the page is behind
// when the tree it shows is not the tree GET changes answers.
func (a *changesAdapter) GetState() (map[string]any, error) {
	behind := false
	if a.open && a.sid == "A" && (a.tree == "ok" || a.tree == "stale") {
		now, err := a.readChanges()
		if err != nil {
			return nil, err
		}
		behind = !slices.Equal(now, a.treeL)
	}
	return map[string]any{
		"open": a.open, "sid": a.sid, "turn": a.turn, "scope": a.scope,
		"edits": a.edits, "tree": a.tree,
		"eseen":  slices.Contains(editPaths(a.sess), "a"),
		"tagent": slices.Contains(changePaths(a.treeL), "a"),
		"behind": behind,
		"ca":     a.ca, "ch": a.ch, "cn": a.cn,
		"diff":   slices.Clone(a.diff),
		"edited": a.edited,
	}, nil
}

// --- the server's reads

func (a *changesAdapter) get(path string, out any) error {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, a.s.URL+path, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		var e struct{ Error string }
		json.NewDecoder(resp.Body).Decode(&e)
		return fmt.Errorf("GET %s: %d %s", path, resp.StatusCode, e.Error)
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

func (a *changesAdapter) id() string {
	switch a.sid {
	case "N":
		return a.nID
	case "R":
		return a.rID
	}
	return a.aID
}

func (a *changesAdapter) sessPath() string { return "/api/sessions/" + url.PathEscape(a.id()) }

func (a *changesAdapter) readChanges() ([]serve.Change, error) {
	var r struct{ Files []serve.Change }
	err := a.get(a.sessPath()+"/changes", &r)
	return r.Files, err
}

// readEdits is useChanges' GET edits, and GET edits?turn= on a turn's page.
func (a *changesAdapter) readEdits() error {
	if a.sid == "A" {
		if err := a.ensureA(); err != nil {
			return err
		}
	}
	var r struct{ Files []serve.Edit }
	if err := a.get(a.sessPath()+"/edits", &r); err != nil {
		return err
	}
	a.sess = r.Files
	if a.turn {
		var t struct{ Files []serve.Edit }
		if err := a.get(fmt.Sprintf("%s/edits?turn=%d", a.sessPath(), a.turnSeq), &t); err != nil {
			return err
		}
		a.turnL = t.Files
	}
	a.edits = "ok"
	return nil
}

func (a *changesAdapter) readTree() error {
	if a.sid == "A" {
		if err := a.ensureA(); err != nil {
			return err
		}
	}
	fs, err := a.readChanges()
	if err != nil {
		return err
	}
	a.treeL, a.tree = fs, "ok"
	return nil
}

func editPaths(es []serve.Edit) []string {
	out := []string{}
	for _, e := range es {
		out = append(out, e.Path)
	}
	return out
}

func changePaths(cs []serve.Change) []string {
	out := []string{}
	for _, c := range cs {
		out = append(out, c.Path)
	}
	return out
}

// --- the page, as the spec's helpers have it, on the server's lists

// files is the list scope s shows, or nil while it has no data.
func (a *changesAdapter) files(s string) []string {
	st := a.edits
	if s == "tree" {
		st = a.tree
	}
	if st == "none" || st == "loading" || st == "failed" {
		return nil
	}
	switch s {
	case "tree":
		return changePaths(a.treeL)
	case "turn":
		return editPaths(a.turnL)
	}
	return editPaths(a.sess)
}

func (a *changesAdapter) shown() []string {
	if !a.open {
		return []string{}
	}
	if fs := a.files(a.scope); fs != nil {
		return fs
	}
	return []string{}
}

func (a *changesAdapter) card(f string) bool {
	switch f {
	case "a":
		return a.ca
	case "h":
		return a.ch
	}
	return a.cn
}

// patched is the page's Edit.patch: the tree always diffs, a session or
// turn edit only when the server says it has a patch.
func (a *changesAdapter) patched(f string) bool {
	if a.scope == "tree" {
		return true
	}
	l := a.sess
	if a.scope == "turn" {
		l = a.turnL
	}
	for _, e := range l {
		if e.Path == f {
			return e.Patch
		}
	}
	return false
}

// settle is the spec's settled(): cards once the list may have changed,
// then each open, patched card's diff fetched from the server. A card
// whose diff comes back empty has nothing on screen.
func (a *changesAdapter) settle(before []string, remount bool) error {
	after := a.shown()
	c := map[string]bool{}
	for _, f := range []string{"a", "h", "n"} {
		switch {
		case !slices.Contains(after, f):
			c[f] = false
		case slices.Contains(before, f) && !remount:
			c[f] = a.card(f)
		default:
			c[f] = len(after) == 1
		}
	}
	a.ca, a.ch, a.cn = c["a"], c["h"], c["n"]
	a.diff = []string{}
	for _, f := range after {
		if !c[f] || !a.patched(f) {
			continue
		}
		q := url.Values{"path": {f}, "scope": {a.scope}}
		if a.scope == "turn" {
			q.Set("turn", fmt.Sprint(a.turnSeq))
		}
		var r struct{ Diff string }
		if err := a.get(a.sessPath()+"/diff?"+q.Encode(), &r); err != nil {
			return err
		}
		if r.Diff != "" {
			a.diff = append(a.diff, f)
		}
	}
	return nil
}

func (a *changesAdapter) tabs() []string {
	if a.turn {
		return []string{"turn", "session", "tree"}
	}
	return []string{"session", "tree"}
}

func (a *changesAdapter) canSwitch(s string) bool {
	if !a.open || s == a.scope || !slices.Contains(a.tabs(), s) || a.edits == "loading" || a.behindNow() {
		return false
	}
	fs := a.files(s)
	return fs == nil || len(fs) > 0
}

// behindNow is GetState's behind, for a require.
func (a *changesAdapter) behindNow() bool {
	st, err := a.GetState()
	return err == nil && st["behind"].(bool)
}

// --- actions

func (a *changesAdapter) SelectNext() error {
	if a.gate.pass(!a.open) {
		a.sid = map[string]string{"A": "N", "N": "R", "R": "A"}[a.sid]
	}
	return nil
}

func (a *changesAdapter) Open() error {
	if a.gate.pass(!a.open) {
		a.open, a.scope, a.edits, a.tree = true, "session", "loading", "loading"
	}
	return nil
}

func (a *changesAdapter) OpenTurn() error {
	if a.gate.pass(!a.open && a.sid == "A" && a.edited) {
		a.open, a.turn, a.scope, a.edits, a.tree = true, true, "turn", "loading", "loading"
	}
	return nil
}

func (a *changesAdapter) Close() error {
	if a.gate.pass(a.open && a.edits != "loading" && a.tree != "stale" && !a.behindNow()) {
		a.reset()
	}
	return nil
}

func (a *changesAdapter) to(s string) error {
	if !a.gate.pass(a.canSwitch(s)) {
		return nil
	}
	a.scope = s
	return a.settle(nil, true)
}

func (a *changesAdapter) ToTurn() error    { return a.to("turn") }
func (a *changesAdapter) ToSession() error { return a.to("session") }
func (a *changesAdapter) ToTree() error    { return a.to("tree") }

func (a *changesAdapter) ExpandA() error {
	if !a.gate.pass(slices.Contains(a.shown(), "a") && !a.ca && !a.behindNow()) {
		return nil
	}
	a.ca = true
	return a.settle(a.shown(), false)
}

// rereadAll is Retry and the transcript tick: every read again.
func (a *changesAdapter) rereadAll() error {
	before := a.shown()
	if err := a.readEdits(); err != nil {
		return err
	}
	if err := a.readTree(); err != nil {
		return err
	}
	return a.settle(before, false)
}

func (a *changesAdapter) Retry() error {
	failed := (a.scope == "tree" && (a.tree == "failed" || a.tree == "stale")) || (a.scope != "tree" && a.edits == "failed")
	if !a.gate.pass(a.open && a.edits != "loading" && !a.behindNow() && failed) {
		return nil
	}
	return a.rereadAll()
}

func (a *changesAdapter) Answer() error {
	if !a.gate.pass(a.edits == "loading") {
		return nil
	}
	if err := a.readEdits(); err != nil {
		return err
	}
	if err := a.readTree(); err != nil {
		return err
	}
	return a.settle(nil, true)
}

func (a *changesAdapter) AnswerFails() error {
	if a.gate.pass(a.edits == "loading" && a.sid == "A" && !a.turn && !a.edited) {
		a.edits, a.tree = "failed", "failed"
	}
	return nil
}

func (a *changesAdapter) Poll() error {
	if !a.gate.pass(a.open && a.edits != "loading" && (a.tree != "ok" || a.behindNow())) {
		return nil
	}
	before := a.shown()
	if err := a.readTree(); err != nil {
		return err
	}
	return a.settle(before, false)
}

func (a *changesAdapter) PollFails() error {
	if a.gate.pass(a.open && a.sid == "A" && !a.turn && a.scope == "tree" && a.tree == "ok" && a.edits == "ok" && !a.behindNow()) {
		a.tree = "stale"
	}
	return nil
}

// AgentEdit is a real turn of A's: its first response is a bash call
// that writes "a" (no write tool reports it), its second a reply. A page
// open on A then re-reads, as the transcript tick does.
func (a *changesAdapter) AgentEdit() error {
	settled := !a.open || (a.edits == "ok" && a.tree == "ok" && !a.behindNow())
	if !a.gate.pass(!a.edited && settled) {
		return nil
	}
	if err := a.ensureA(); err != nil {
		return err
	}
	a.n++
	name := fmt.Sprintf("t%05d", a.n)
	if a.outOfBand {
		control.Queue(a.t, a.dir, name+"a", control.Turn{Mode: "block", Text: "edited"})
	} else {
		control.Queue(a.t, a.dir, name+"a", control.Turn{Mode: "ok", Calls: []control.Call{{Name: "bash", Args: map[string]any{"command": "printf 'agent\\n' > a"}}}})
		control.Queue(a.t, a.dir, name+"b", control.Turn{Mode: "ok", Text: "edited a"})
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.aID, "edit a "+name); err != nil {
		return err
	}
	if a.outOfBand {
		control.WaitTaken(a.t, a.dir, name+"a", actionTimeout)
		if err := os.WriteFile(filepath.Join(a.aDir, "a"), []byte("agent\n"), 0o644); err != nil {
			return err
		}
		control.Release(a.t, a.dir, name+"a")
	}
	lines, err := a.waitDone(a.aID, 1)
	if err != nil {
		return err
	}
	for _, l := range lines {
		if l.Kind == "input" {
			a.turnSeq = l.Seq
		}
	}
	a.edited = true
	if a.open && a.sid == "A" && a.edits != "loading" {
		return a.rereadAll()
	}
	return nil
}

// HandEdit appends a line to A's "h" outside bough: no entry, no tick.
func (a *changesAdapter) HandEdit() error {
	if !a.gate.pass(a.open && a.sid == "A" && a.scope == "tree" && a.tree == "ok" && !a.behindNow()) {
		return nil
	}
	f, err := os.OpenFile(filepath.Join(a.aDir, "h"), os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintln(f, "by hand")
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}

var changesActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"SelectNext":  action((*changesAdapter).SelectNext),
	"Open":        action((*changesAdapter).Open),
	"OpenTurn":    action((*changesAdapter).OpenTurn),
	"Close":       action((*changesAdapter).Close),
	"ToTurn":      action((*changesAdapter).ToTurn),
	"ToSession":   action((*changesAdapter).ToSession),
	"ToTree":      action((*changesAdapter).ToTree),
	"ExpandA":     action((*changesAdapter).ExpandA),
	"Retry":       action((*changesAdapter).Retry),
	"Answer":      action((*changesAdapter).Answer),
	"AnswerFails": action((*changesAdapter).AnswerFails),
	"Poll":        action((*changesAdapter).Poll),
	"PollFails":   action((*changesAdapter).PollFails),
	"AgentEdit":   action((*changesAdapter).AgentEdit),
	"HandEdit":    action((*changesAdapter).HandEdit),
}}

// The runner picks among all 15 actions, disabled ones included, and a
// walk is only checked up to its first disabled one, so most walks are
// short: many cheap walks (a disabled step costs nothing, A is made on
// first use) rather than a few long ones.
func changesOptions() map[string]any {
	return map[string]any{"max-seq-runs": 1000, "max-actions": 8, "max-parallel-runs": 0}
}

// changesHistory reads A's transcript: each turn is an AgentEdit when
// its done recorded "a" (the shell edit, from the checkpoint diff), and
// something the model has no action for when it did not.
func changesHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Page#0.edited": false}}}
	for _, e := range entries {
		if e.Kind != "done" {
			continue
		}
		fs, _ := e.Data["files"].([]any)
		if slices.Contains(fs, any("a")) {
			steps = append(steps, tracecheck.Step{Action: "Page#0.AgentEdit", State: map[string]any{"Page#0.edited": true}})
		} else {
			steps = append(steps, tracecheck.Step{Action: fmt.Sprintf("a turn that recorded %v, not the shell edit", fs)})
		}
	}
	return steps
}

func init() { historyProjections["changes_review"] = changesHistory }

func TestChangesReview(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newChangesAdapter(t)
	if err := runMBT(t, "changes_review", a, changesActions, changesOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "changes_review"))
	if err != nil {
		t.Fatal(err)
	}
	edited := 0
	for _, id := range a.aIDs {
		entries := sessionHistory(t, a.s.Home, id)
		checkHistory(t, g, entries, changesHistory)
		if len(changesHistory(entries)) > 1 {
			edited++
		}
	}
	if edited == 0 {
		t.Errorf("no walk ran AgentEdit (%d A sessions); the trace check checked nothing", len(a.aIDs))
	}
}

// The projection must be able to reject a transcript: a turn whose done
// did not record the shell edit, or a second edit, is not a path.
func TestChangesReviewHistoryRejects(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	g, err := tracecheck.Load(fizzCheck(t, "changes_review"))
	if err != nil {
		t.Fatal(err)
	}
	turn := func(files ...any) []history.Entry {
		return []history.Entry{{Kind: "input"}, {Kind: "done", Data: map[string]any{"files": files}}}
	}
	if v := g.Check(changesHistory(turn("a"))); v != nil {
		t.Fatalf("one shell edit of a: %v", v)
	}
	for name, es := range map[string][]history.Entry{
		"a turn that recorded nothing": turn(),
		"two edits":                    append(turn("a"), turn("a")...),
	} {
		if g.Check(changesHistory(es)) == nil {
			t.Errorf("%s: accepted as a path in the model", name)
		}
	}
}

// An agent edit the session's own calls did not make must fail the run:
// here the adapter writes "a" itself while the turn is held, so the
// turn ran no shell, and its done records nothing.
func TestChangesReviewCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newChangesAdapter(t)
	a.outOfBand = true
	// The bug only shows on a walk that edits and then reads A's edits
	// (three picks out of fifteen in a row), so 1000 walks missed it
	// about a third of the time; walks that stop early cost nothing.
	opts := changesOptions()
	opts["max-seq-runs"] = 8000
	if err := runMBT(t, "changes_review", a, changesActions, opts); err == nil {
		t.Fatal("a run whose agent edit no call made passed; the runner is not checking state")
	}
}
