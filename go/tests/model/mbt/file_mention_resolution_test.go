//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/file_mention_resolution.fizz against a real serve: an "@" picked
// in the composer, through Send, to what the child's admit attaches.
//
// The picker's own state (picker, stale, shown) is the page's, which the
// adapter plays the way mention.tsx does; the page-only promises (a hung
// read timing out, a directory row saying it attaches nothing, a stale
// reply dropped) are the browser stage's to show. What the server
// decides is read off it:
//
//   - orb: the open session's row says mode "project". Project sessions
//     run on the fake container runtime (the orb row's `runtime: fake`),
//     so the child is a host process that chdirs into its orb's primary
//     dir, exactly as a real one does before its container starts.
//   - searched: GET /api/files?session=<id>&q=<token> is really sent,
//     and its rows say which root it searched: the fixture files live
//     only in the root the child expands against (the local session's
//     cwd, or the orb's primary), so rows naming them bare came from
//     there, and anything else did not.
//   - dirNoted: the directory row carries dir:true, the one thing the
//     page can say "attaches nothing" from.
//   - msg: off the transcript. The child records the input it admitted,
//     with every "[file: ...]" block ExpandAt added.
//
// ReplyFails is the server refusing the read (a wrong token), and
// ServerHangs is the world's choice a healthy serve cannot make: the
// adapter sends nothing for it, and TimedOut is the page's give-up.

// The fixture, the same names in every expand root. A shared prefix
// keeps every token TypeToken types matching all three.
const (
	fmrPrefix = "fmrfixture-"
	fmrFile   = fmrPrefix + "note.txt"
	fmrBig    = fmrPrefix + "big.log" // over ExpandAt's 64 KiB
	fmrDir    = fmrPrefix + "dir"
)

// The project and its one repo, under the serve's HOME.
const (
	fmrSlug = "mentions"
	fmrRepo = "src/mentions"
)

// fmrConfig is llm-control for the turns and the fake container runtime
// for project sessions.
const fmrConfig = controlConfig + "- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"

type fmrSession struct {
	id   string
	used bool // sent to: its transcript has a message, so a walk needs a new one
}

type fmrAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	work string // every local session's cwd
	slug string // the project the orb sessions belong to
	gate gate

	// pool["local"|"orb"][0|1] is session a and session b of that kind,
	// reused across walks until one is sent to.
	pool map[string]*[2]fmrSession
	ids  []string // every session made, for the trace check
	turn int

	// The Mention role, as the page holds it.
	view, picker, shown, searched, picked, msg string
	orb, stale, dirNoted, admitted             bool

	token string   // the typed token after "@"
	rows  []fmrRow // the picker's rows
	path  string   // what the pick put in the draft
	cur   *fmrSession
	late  chan error // the abandoned read, still on the wire

	// noSession is the deliberate bug the CatchesWrongAdapter tests
	// inject: the picker asks without naming the session, so serve
	// searches its own directory.
	noSession bool

	// What the checked steps reached, logged at the end.
	sent map[string]int
}

type fmrRow struct {
	Path string `json:"path"`
	Dir  bool   `json:"dir"`
}

func newFMRAdapter(t *testing.T) *fmrAdapter {
	s := servetest.Start(t, servetest.Options{Config: fmrConfig, Files: map[string]string{
		".bough/projects/" + fmrSlug + "/project.yml": "name: Mentions\nrepos:\n  - path: ~/" + fmrRepo + "\n",
	}})
	a := &fmrAdapter{t: t, s: s, dir: control.Dir(s.Home), work: s.Dir(t, "work"), slug: fmrSlug,
		pool: map[string]*[2]fmrSession{"local": {}, "orb": {}}, sent: map[string]int{}}
	if err := fmrFixture(a.work); err != nil {
		t.Fatal(err)
	}
	// The project's one repo holds the fixture committed, so each orb's
	// primary worktree (orbs/<id>/<repo>, not the orb dir itself) has it.
	repo := filepath.Join(s.Home, fmrRepo)
	if err := fmrFixture(repo); err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{
		{"init", "--quiet", "--initial-branch=main"},
		{"add", "."},
		{"-c", "user.name=fmr", "-c", "user.email=fmr@example.invalid", "commit", "--quiet", "-m", "fixture"},
	} {
		cmd := exec.Command("git", append([]string{"-C", repo}, args...)...)
		cmd.Env = append(os.Environ(), "HOME="+s.Home, "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=/dev/null")
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	return a
}

// fmrFixture writes the three things a pick can name into root.
func fmrFixture(root string) error {
	if err := os.MkdirAll(filepath.Join(root, fmrDir), 0o755); err != nil {
		return err
	}
	for name, body := range map[string]string{
		fmrFile:                     "the note's words\n",
		fmrBig:                      strings.Repeat("0123456789abcdef\n", 70*1024/17),
		path.Join(fmrDir, "in.txt"): "inside the directory\n",
	} {
		if err := os.WriteFile(filepath.Join(root, name), []byte(body), 0o644); err != nil {
			return err
		}
	}
	return nil
}

func fmrDo(ctx context.Context, s *servetest.Server, method, p, tok string, body, out any) error {
	var rd *strings.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = strings.NewReader(string(b))
	} else {
		rd = strings.NewReader("")
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+p, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+tok)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		return fmt.Errorf("%s %s: %s", method, p, resp.Status)
	}
	if out == nil {
		return nil
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// Init starts a walk with no session open. Sessions a walk sent to are
// replaced; the rest are reused, since most walks end in a step or two.
func (a *fmrAdapter) Init() error {
	a.drainLate()
	for _, p := range a.pool {
		for i := range p {
			if p[i].used {
				p[i] = fmrSession{}
			}
		}
	}
	a.view, a.picker, a.shown, a.searched, a.picked, a.msg = "none", "none", "", "", "none", "none"
	a.orb, a.stale, a.dirNoted, a.admitted = false, false, false, false
	a.token, a.rows, a.path, a.cur = "", nil, "", nil
	a.gate.reset()
	return nil
}

// Cleanup collects a read the walk abandoned, so it cannot land in the
// next one.
func (a *fmrAdapter) Cleanup() error { return a.drainLate() }

func (a *fmrAdapter) drainLate() error {
	if a.late == nil {
		return nil
	}
	err := <-a.late
	a.late = nil
	return err
}

func (a *fmrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Mention", Index: 0}: a}, nil
}

// GetState reads orb off the open session's row and msg off its
// transcript; the rest is the page's, as the actions left it.
func (a *fmrAdapter) GetState() (map[string]any, error) {
	orb, msg := a.orb, "none"
	if a.cur != nil {
		ctx, cancel := actionCtx()
		defer cancel()
		row, lines, err := a.s.GetSession(ctx, a.cur.id)
		if err != nil {
			return nil, err
		}
		orb = row.Mode == "project"
		msg = a.msgOf(lines)
	}
	return map[string]any{
		"view": a.view, "orb": orb, "picker": a.picker, "stale": a.stale,
		"shown": a.shown, "searched": a.searched, "picked": a.picked,
		"dirNoted": a.dirNoted, "msg": msg,
	}, nil
}

// msgOf is the spec's msg from the transcript: "sent" once the input is
// there and the walk has not yet taken the ExpandAt step (the child
// admits as it records, so the two are one moment on the server), then
// what the admitted input carries.
func (a *fmrAdapter) msgOf(lines []serve.Line) string {
	var in *serve.Line
	for i := range lines {
		if lines[i].Kind == "input" {
			in = &lines[i]
		}
	}
	switch {
	case in == nil:
		return "none"
	case !a.admitted:
		return "sent"
	}
	return fmrExpanded(in.Text, a.path)
}

// fmrExpanded classifies an admitted input by the block ExpandAt added
// for p: whole, cut with its note, or none.
func fmrExpanded(text, p string) string {
	_, block, ok := strings.Cut(text, "\n\n[file: "+p+"]\n")
	switch {
	case !ok:
		return "not_expanded"
	case strings.Contains(block, "\n[truncated at "):
		return "truncated"
	}
	return "expanded"
}

// open makes (or reuses) session i of the kind and makes it the one on
// screen.
func (a *fmrAdapter) open(kind string, i int) error {
	sl := &a.pool[kind][i]
	if sl.id == "" {
		id, err := a.create(kind)
		if err != nil {
			return err
		}
		sl.id = id
		a.ids = append(a.ids, id)
	}
	a.cur = sl
	return nil
}

// create starts a session of kind and waits until it can take a message
// and its expand root holds the fixture: a project session's orb dir is
// made by its child, so the fixture is written there once it exists.
func (a *fmrAdapter) create(kind string) (string, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var id string
	if kind == "local" {
		row, err := a.s.CreateSession(ctx, a.work, "")
		if err != nil {
			return "", err
		}
		id = row.ID
	} else {
		var r struct {
			Session serve.Row `json:"session"`
		}
		if err := fmrDo(ctx, a.s, http.MethodPost, "/api/sessions", a.s.Token, map[string]any{"mode": "project", "project": a.slug}, &r); err != nil {
			return "", err
		}
		id = r.Session.ID
	}
	for {
		row, _, err := a.s.GetSession(ctx, id)
		if err == nil && row.Live && row.Mode != "" {
			if kind == "local" {
				return id, nil
			}
			// The child chdirs into its primary worktree before it
			// takes a message; state.json names it once it exists.
			if st, err := iorb.ReadState(a.s.Home, id); err == nil && st.Primary != "" {
				return id, nil
			}
		}
		select {
		case <-ctx.Done():
			return "", fmt.Errorf("%s session %s never became ready: %w", kind, id, ctx.Err())
		case <-time.After(50 * time.Millisecond):
		}
	}
}

// filesPath is the page's read (mention.tsx useFiles).
func (a *fmrAdapter) filesPath() string {
	p := "/api/files?q=" + url.QueryEscape(a.token)
	if !a.noSession {
		p += "&session=" + url.QueryEscape(a.cur.id)
	}
	return p
}

func (a *fmrAdapter) readFiles(tok string) ([]fmrRow, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	var body struct {
		Files []fmrRow `json:"files"`
	}
	err := fmrDo(ctx, a.s, http.MethodGet, a.filesPath(), tok, nil, &body)
	return body.Files, err
}

// abandon is a read left on the wire: sent now, collected by StaleReply
// (or the next walk), and never shown.
func (a *fmrAdapter) abandon() error {
	if err := a.drainLate(); err != nil {
		return err
	}
	p, done := a.filesPath(), make(chan error, 1)
	go func() {
		ctx, cancel := actionCtx()
		defer cancel()
		var sink any
		done <- fmrDo(ctx, a.s, http.MethodGet, p, a.s.Token, nil, &sink)
	}()
	a.late, a.stale = done, true
	return nil
}

func (a *fmrAdapter) clearRows() { a.shown, a.searched, a.rows = "", "", nil }

// --- which session is open -----------------------------------------------

func (a *fmrAdapter) OpenLocal() error {
	if !a.gate.pass(a.view == "none") {
		return nil
	}
	a.view, a.orb = "a", false
	return a.open("local", 0)
}

func (a *fmrAdapter) OpenOrb() error {
	if !a.gate.pass(a.view == "none") {
		return nil
	}
	a.view, a.orb = "a", true
	return a.open("orb", 0)
}

// --- the picker ----------------------------------------------------------

func (a *fmrAdapter) TypeAt() error {
	if a.gate.pass(a.view != "none" && a.picker == "none" && a.picked == "none" && a.msg == "none") {
		a.picker, a.token = "debouncing", ""
	}
	return nil
}

// Debounced: the request goes. Which answer it gets is the next step's.
func (a *fmrAdapter) Debounced() error {
	if a.gate.pass(a.picker == "debouncing") {
		a.picker = "fetching"
	}
	return nil
}

// ReplyArrives shows the rows and says where they were searched: under
// the expand root when the fixture's names are there bare.
func (a *fmrAdapter) ReplyArrives() error {
	if !a.gate.pass(a.picker == "fetching") {
		return nil
	}
	rows, err := a.readFiles(a.s.Token)
	if err != nil {
		return err
	}
	a.rows, a.picker, a.shown, a.searched = rows, "results", "current", "other"
	names := map[string]bool{}
	for _, r := range rows {
		names[r.Path] = true
	}
	if names[fmrFile] && names[fmrBig] && names[fmrDir] {
		a.searched = "expand_root"
	}
	return nil
}

func (a *fmrAdapter) ReplyFails() error {
	if !a.gate.pass(a.picker == "fetching") {
		return nil
	}
	a.picker = "error"
	if _, err := a.readFiles("not-" + a.s.Token); err == nil {
		return fmt.Errorf("GET %s with a wrong token succeeded", a.filesPath())
	}
	return nil
}

func (a *fmrAdapter) ServerHangs() error {
	if a.gate.pass(a.picker == "fetching") {
		a.picker = "hung"
	}
	return nil
}

func (a *fmrAdapter) TimedOut() error {
	if a.gate.pass(a.picker == "hung") {
		a.picker = "error"
	}
	return nil
}

func (a *fmrAdapter) Retry() error {
	if a.gate.pass(a.picker == "error") {
		a.picker = "debouncing"
	}
	return nil
}

// TypeToken types the next letter of the fixture's shared prefix.
func (a *fmrAdapter) TypeToken() error {
	if !a.gate.pass(a.picker != "none") {
		return nil
	}
	if len(a.token) == len(fmrPrefix) {
		return fmt.Errorf("TypeToken: the token %q is the whole shared prefix; a longer walk needs a longer one", a.token)
	}
	if a.picker == "fetching" {
		if err := a.abandon(); err != nil {
			return err
		}
	}
	a.token += fmrPrefix[len(a.token) : len(a.token)+1]
	a.picker = "debouncing"
	a.clearRows()
	return nil
}

func (a *fmrAdapter) Dismiss() error {
	if !a.gate.pass(a.picker != "none") {
		return nil
	}
	if a.picker == "fetching" {
		if err := a.abandon(); err != nil {
			return err
		}
	}
	a.picker = "none"
	a.clearRows()
	return nil
}

// SwitchSession opens the other session of the same kind: the composer
// remounts empty there.
func (a *fmrAdapter) SwitchSession() error {
	if !a.gate.pass(a.view == "a" && (a.picker == "debouncing" || a.picker == "fetching" || a.picker == "hung")) {
		return nil
	}
	if a.picker == "fetching" {
		if err := a.abandon(); err != nil {
			return err
		}
	}
	a.view, a.picker, a.picked, a.dirNoted, a.token, a.path = "b", "none", "none", false, "", ""
	a.clearRows()
	kind := "local"
	if a.orb {
		kind = "orb"
	}
	return a.open(kind, 1)
}

// StaleReply: the abandoned read lands and is dropped.
func (a *fmrAdapter) StaleReply() error {
	if !a.gate.pass(a.stale) {
		return nil
	}
	a.stale = false
	return a.drainLate()
}

// pick takes the row whose base name is want, as a click would, and
// puts "@<path>" in the draft. A row searched elsewhere still names the
// file, so a wrong root reaches Send and shows there too.
func (a *fmrAdapter) pick(want, picked string) error {
	for _, r := range a.rows {
		if path.Base(r.Path) == want {
			a.path, a.picked, a.picker = r.Path, picked, "none"
			if picked == "dir" {
				a.dirNoted = r.Dir
			}
			a.clearRows()
			return nil
		}
	}
	return fmt.Errorf("Pick: no row for %s among %v", want, a.rows)
}

func (a *fmrAdapter) PickFile() error {
	if !a.gate.pass(a.picker == "results") {
		return nil
	}
	return a.pick(fmrFile, "file")
}

func (a *fmrAdapter) PickBig() error {
	if !a.gate.pass(a.picker == "results") {
		return nil
	}
	return a.pick(fmrBig, "big")
}

func (a *fmrAdapter) PickDir() error {
	if !a.gate.pass(a.picker == "results") {
		return nil
	}
	return a.pick(fmrDir, "dir")
}

// --- Send, then the child's admit ----------------------------------------

// Send posts the draft and waits for the child to record it and for its
// turn to end, so no turn is left running into the next walk.
func (a *fmrAdapter) Send() error {
	if !a.gate.pass(a.picker == "none" && a.picked != "none" && a.msg == "none") {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("fmr%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "ok", Text: "read " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.cur.id, "look at @"+a.path+" please"); err != nil {
		return err
	}
	a.cur.used, a.msg = true, "sent"
	a.sent[a.picked]++
	for {
		_, lines, err := a.s.GetSession(ctx, a.cur.id)
		if err == nil && slicesHasKind(lines, "done") {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("Send: the turn never ended: %w", ctx.Err())
		case <-time.After(100 * time.Millisecond):
		}
	}
}

func slicesHasKind(lines []serve.Line, kind string) bool {
	for _, l := range lines {
		if l.Kind == kind {
			return true
		}
	}
	return false
}

// ExpandAt is the admit the child already did at Send: from here the
// state reads what it attached.
func (a *fmrAdapter) ExpandAt() error {
	if a.gate.pass(a.msg == "sent") {
		// msg is what the transcript says from here (msgOf); the adapter
		// keeps it only as "past none" for the requires.
		a.admitted, a.msg = true, "admitted"
	}
	return nil
}

var fmrActions = map[string]map[string]fmbt.ActionFunc{"Mention": {
	"OpenLocal":     action((*fmrAdapter).OpenLocal),
	"OpenOrb":       action((*fmrAdapter).OpenOrb),
	"TypeAt":        action((*fmrAdapter).TypeAt),
	"Debounced":     action((*fmrAdapter).Debounced),
	"ReplyArrives":  action((*fmrAdapter).ReplyArrives),
	"ReplyFails":    action((*fmrAdapter).ReplyFails),
	"ServerHangs":   action((*fmrAdapter).ServerHangs),
	"TimedOut":      action((*fmrAdapter).TimedOut),
	"Retry":         action((*fmrAdapter).Retry),
	"TypeToken":     action((*fmrAdapter).TypeToken),
	"Dismiss":       action((*fmrAdapter).Dismiss),
	"SwitchSession": action((*fmrAdapter).SwitchSession),
	"StaleReply":    action((*fmrAdapter).StaleReply),
	"PickFile":      action((*fmrAdapter).PickFile),
	"PickBig":       action((*fmrAdapter).PickBig),
	"PickDir":       action((*fmrAdapter).PickDir),
	"Send":          action((*fmrAdapter).Send),
	"ExpandAt":      action((*fmrAdapter).ExpandAt),
}, "": {
	// deadlock_detection is off, so fizz links the admitted end state to
	// itself as "end" and the runner offers it everywhere. It is a step
	// only there (nothing left to do, no reply still on the wire); taken
	// anywhere else it is a disabled pick, and the gate closes.
	"end": action((*fmrAdapter).End),
}}

func (a *fmrAdapter) End() error {
	a.gate.pass(a.admitted && !a.stale)
	return nil
}

// Most steps are the page's and a session is reused until a Send, so
// walks are cheap; the runner stops checking at the first disabled one
// of 18 actions, so it takes many to get past a reply.
func fmrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 3000, "max-actions": 8, "max-parallel-runs": 0}
}

// fmrHistory reads a session's transcript as a path. Only the meta
// (which kind of session) and the input (what was picked, what admit
// attached) are on record, so the steps between are the shortest way to
// the pick the input names; the checked states are orb, picked and msg.
func fmrHistory(entries []history.Entry) []tracecheck.Step {
	q := func(kv ...any) map[string]any {
		m := map[string]any{}
		for i := 0; i < len(kv); i += 2 {
			m["Mention#0."+kv[i].(string)] = kv[i+1]
		}
		return m
	}
	steps := []tracecheck.Step{{Action: "Init", State: q("view", "none", "msg", "none")}}
	for _, e := range entries {
		switch e.Kind {
		case "meta":
			if mode, _ := e.Data["mode"].(string); mode == "project" {
				steps = append(steps, tracecheck.Step{Action: "Mention#0.OpenOrb", State: q("orb", true)})
			} else {
				steps = append(steps, tracecheck.Step{Action: "Mention#0.OpenLocal", State: q("orb", false)})
			}
		case "input":
			typed, _ := e.Data["typed"].(string)
			text, _ := e.Data["text"].(string)
			if typed == "" {
				typed = text
			}
			var p string
			for _, w := range strings.Fields(typed) {
				if s, ok := strings.CutPrefix(w, "@"); ok {
					p = s
				}
			}
			action, picked := "PickFile", "file"
			switch path.Base(p) {
			case fmrBig:
				action, picked = "PickBig", "big"
			case fmrDir:
				action, picked = "PickDir", "dir"
			}
			steps = append(steps,
				tracecheck.Step{Action: "Mention#0.TypeAt"},
				tracecheck.Step{Action: "Mention#0.Debounced"},
				tracecheck.Step{Action: "Mention#0.ReplyArrives"},
				tracecheck.Step{Action: "Mention#0." + action, State: q("picked", picked)},
				tracecheck.Step{Action: "Mention#0.Send", State: q("msg", "sent")},
				tracecheck.Step{Action: "Mention#0.ExpandAt", State: q("msg", fmrExpanded(text, p))})
			return steps
		}
	}
	return steps
}

func init() { historyProjections["file_mention_resolution"] = fmrHistory }

// checkHistories replays every transcript the walks wrote on g, and
// with MODEL_TRACE_DIR set keeps them there for TestHistoryTraces.
func (a *fmrAdapter) checkHistories(t *testing.T, g *tracecheck.Graph) {
	t.Helper()
	keep := os.Getenv("MODEL_TRACE_DIR")
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), fmrHistory)
		if keep == "" {
			continue
		}
		b, err := os.ReadFile(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		dir := filepath.Join(keep, "file_mention_resolution")
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, "go-"+id+".jsonl"), b, 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

// TestFileMentionResolution lets fizzbee-mbt walk the spec at random.
// It rarely strings a reply, a pick and a Send together;
// TestFileMentionResolutionPaths is the cover.
func TestFileMentionResolution(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newFMRAdapter(t)
	if err := runMBT(t, "file_mention_resolution", a, fmrActions, fmrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	t.Logf("checked steps sent %v", a.sent)
	g, err := tracecheck.Load(fizzCheck(t, "file_mention_resolution"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkHistories(t, g)
}

// TestFileMentionResolutionPaths walks the paths derived from the
// checked-in graph against one serve and compares every step's state.
// It needs neither fizz nor the MBT lock.
func TestFileMentionResolutionPaths(t *testing.T) {
	t.Parallel()
	a := newFMRAdapter(t)
	if err := walkFMRPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	t.Logf("sent %v", a.sent)
	for _, k := range []string{"file", "big", "dir"} {
		if a.sent[k] == 0 {
			t.Errorf("the paths never sent a %s pick (sent %v)", k, a.sent)
		}
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "file_mention_resolution"))
	if err != nil {
		t.Fatal(err)
	}
	a.checkHistories(t, g)
}

// With the picker asking without the session, serve searches its own
// directory and some step must disagree.
func TestFileMentionResolutionPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newFMRAdapter(t)
	a.noSession = true
	if walkFMRPaths(t, a, envCover()) == nil {
		t.Fatal("every path passed with a picker that ignores the session; the walk is not checking state")
	}
}

// walkFMRPaths runs every path, each to its first divergence, and
// reports them all.
func walkFMRPaths(t *testing.T, a *fmrAdapter, cover tracecheck.Cover) error {
	t.Helper()
	raw, err := pathsJSONCover("file_mention_resolution", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil || len(doc.Paths) == 0 {
		t.Fatalf("paths: %v (%d paths)", err, len(doc.Paths))
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Mention#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *fmrAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil {
			err = cerr
		}
	}()
	for j, st := range trace {
		if st.Action == "Init" {
			err = a.Init()
		} else {
			name := strings.TrimPrefix(st.Action, "Mention#0.")
			act, ok := fmrActions["Mention"][name]
			if name == "end" {
				act, ok = fmrActions[""][name]
			}
			if !ok {
				return fmt.Errorf("step %d: no action %q", j, st.Action)
			}
			_, err = act(a, nil)
			if err == nil && a.gate.off {
				err = errors.New("disabled in the adapter's view")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", j, st.Action, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j, st.Action, err)
		}
		for k, want := range st.State {
			if f, ok := strings.CutPrefix(k, "Mention#0."); ok && got[f] != want {
				return fmt.Errorf("step %d (%s): %s is %v, the spec says %v (state %v)", j, st.Action, f, got[f], want, got)
			}
		}
	}
	return nil
}
