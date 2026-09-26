//go:build !windows

package mbt

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servepid"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/wiki_redact_vs_ingest_race.fizz against a real serve: a
// hypothetical "scrub this page" pass (read-modify-write, no lock) racing
// the real ingest tick's own read-modify-write of the same page.
//
// The redact side calls the real page-write API (PUT /api/wiki/page,
// which commits synchronously: store.go's WritePage), so its two steps
// are genuinely atomic from the walk's view, matching the spec's atomic
// RedactRead/RedactWrite. The ingest side is the real `bough wiki run`
// (run.go), held mid-turn by llm-control and mid-commit by a commit-msg
// hook exactly as wiki_edit_vs_ingest_lock_test.go does, so its own
// read (reaching the held turn) and write (the agent's edit + parked
// commit) are two steps a walk can put a redact write between.

const wrrPage = "topics/demo/gamma.md"

// wrrSecret and wrrRedacted are what redact() would do to a real secret:
// the exact string does not matter to this model, only that a walk can
// tell whether the page currently carries it.
const (
	wrrSecret   = "sk-AAAAAAAAAAAAAAAAAAAA"
	wrrRedacted = "[redacted]"
	wrrAppended = "Appended notes."
)

// wrrBody is the page's whole text for a given (secret still present,
// ingest's excerpt appended) pair: both writers replace the page whole,
// so the page is always exactly one of these four bodies.
func wrrBody(old, appended bool) string {
	secret := wrrRedacted
	if old {
		secret = wrrSecret
	}
	b := "# Gamma\n\nSecret: " + secret + "\n"
	if appended {
		b += "\n" + wrrAppended + "\n"
	}
	return b
}

// wrrHook parks an ingest run's commit (subject "ingest ...") with the
// index held, exactly as wiki_edit_vs_ingest_lock_test.go's welHook: a
// redact pass's write goes through WritePage's own synchronous commit
// ("edit ...") and is never parked, so it is atomic the way the spec's
// RedactWrite is.
const wrrHook = `#!/bin/sh
case "$(head -n 1 "$1")" in
"ingest "*)
  : > "%[1]s/committing"
  n=0
  while [ ! -e "%[1]s/release" ] && [ $n -lt 6000 ]; do sleep 0.02; n=$((n+1)); done
  rm -f "%[1]s/committing" "%[1]s/release"
  ;;
esac
exit 0
`

// wrrRun is the ingest run holding the wiki lock.
type wrrRun struct {
	turn  string
	alive func() bool
}

type wikiRedactAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	wiki string // HOME/.bough/wiki
	mark string // the commit hook's handshake dir
	pid  int
	gate gate

	rPhase   string // "idle" or "reading"
	rSnapOld bool
	rSnapApp bool

	iPhase   string // "idle" or "reading"
	iSnapOld bool
	run      *wrrRun

	resurrected bool
	lostIngest  bool

	turn  int
	pends []string // pending sessions this walk wrote

	// bug is a deliberate wiring bug TestWikiRedactVsIngestRaceCatchesWrongAdapter
	// injects: RedactWrite writes from a fresh read instead of the stale
	// snapshot, hiding the race.
	bug string

	ran map[string]int
}

func newWikiRedactAdapter(t *testing.T) *wikiRedactAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: wrrGitEnv()})
	a := &wikiRedactAdapter{t: t, s: s, dir: control.Dir(s.Home), wiki: filepath.Join(s.Home, ".bough", "wiki"), mark: filepath.Join(s.Root, "wiki-hook"), ran: map[string]int{}}
	b, err := os.ReadFile(filepath.Join(s.Home, ".bough", "serve.pid"))
	if err != nil {
		t.Fatal(err)
	}
	if a.pid, _, _, _, _, err = servepid.Parse(string(b)); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(a.mark, 0o755); err != nil {
		t.Fatal(err)
	}
	return a
}

func wrrGitEnv() []string {
	return []string{"GIT_CONFIG_GLOBAL=" + os.DevNull, "GIT_CONFIG_NOSYSTEM=1"}
}

func (a *wikiRedactAdapter) git(args ...string) (string, error) {
	cmd := exec.Command("git", append([]string{"--no-optional-locks", "-C", a.wiki}, args...)...)
	cmd.Env = append(os.Environ(), append(wrrGitEnv(), "HOME="+a.s.Home)...)
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	if err := cmd.Run(); err != nil {
		return out.String(), fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, errb.String())
	}
	return out.String(), nil
}

func (a *wikiRedactAdapter) path(rel string) string {
	return filepath.Join(a.wiki, filepath.FromSlash(rel))
}

// Init reseeds the wiki: gamma.md with the secret, nothing appended, a
// profile and today's brief (so a tick never briefs instead), a log
// baseline that predates the pending sessions this walk writes, and the
// commit hook.
func (a *wikiRedactAdapter) Init() error {
	for _, id := range a.pends {
		if err := os.Remove(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")); err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	a.rPhase, a.rSnapOld, a.rSnapApp = "idle", false, false
	a.iPhase, a.iSnapOld, a.run = "idle", false, nil
	a.resurrected, a.lostIngest = false, false
	a.pends = nil
	a.gate.reset()
	if err := os.RemoveAll(a.path("topics/demo")); err != nil {
		return err
	}
	files := map[string]string{
		wrrPage:      wrrBody(true, false),
		"index.md":   "# Wiki index\n\n## demo\n\n- [Gamma](" + wrrPage + ") — the demo page\n",
		"log.md":     "# Wiki log\n\n<!-- baseline: " + time.Now().Add(-48*time.Hour).UTC().Format(time.RFC3339) + " -->\n",
		".gitignore": ".ingest.lock\ningest.log\n",
	}
	for rel, body := range files {
		p := a.path(rel)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			return err
		}
	}
	if _, err := os.Stat(filepath.Join(a.wiki, ".git")); err != nil {
		if _, err := a.git("init", "-q"); err != nil {
			return err
		}
		hook := filepath.Join(a.wiki, ".git", "hooks", "commit-msg")
		if err := os.MkdirAll(filepath.Dir(hook), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(hook, []byte(fmt.Sprintf(wrrHook, a.mark)), 0o755); err != nil {
			return err
		}
	}
	for _, f := range []string{"committing", "release"} {
		os.Remove(filepath.Join(a.mark, f))
	}
	if _, err := a.git("add", "-A"); err != nil {
		return err
	}
	_, err := a.git("-c", "user.name=walk", "-c", "user.email=walk@localhost", "commit", "-q", "--allow-empty", "-m", "walk: start")
	return err
}

// Cleanup ends an ingest run the walk left holding the lock.
func (a *wikiRedactAdapter) Cleanup() error {
	r := a.run
	if r == nil {
		return nil
	}
	a.run = nil
	if a.iPhase == "reading" {
		control.ReleaseWith(a.t, a.dir, r.turn, control.Turn{Mode: "error", Error: "walk over"})
	}
	return poll("the walk's ingest run to exit", func() (bool, error) {
		if wrrExists(filepath.Join(a.mark, "committing")) {
			os.WriteFile(filepath.Join(a.mark, "release"), nil, 0o644)
		}
		return !r.alive() && !wrrLockHeld(a.wiki), nil
	})
}

func wrrExists(p string) bool { _, err := os.Stat(p); return err == nil }

func (a *wikiRedactAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Page", Index: 0}: a}, nil
}

// body reads gamma.md straight off disk: both writers (WritePage and
// the ingest agent's own edit) hit the same file, and the adapter must
// see exactly what either one left there.
func (a *wikiRedactAdapter) body() (string, error) {
	b, err := os.ReadFile(a.path(wrrPage))
	if err != nil {
		return "", err
	}
	return string(b), nil
}

func (a *wikiRedactAdapter) diskOld(body string) bool { return strings.Contains(body, wrrSecret) }
func (a *wikiRedactAdapter) diskAppended(body string) bool {
	return strings.Contains(body, wrrAppended)
}

func (a *wikiRedactAdapter) GetState() (map[string]any, error) {
	body, err := a.body()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"diskOld":      a.diskOld(body),
		"diskAppended": a.diskAppended(body),
		"diskNew":      false, // neither writer ever manufactures a secret that was never on disk
		"rPhase":       a.rPhase,
		"rSnapOld":     a.rSnapOld,
		"rSnapApp":     a.rSnapApp,
		"iPhase":       a.iPhase,
		"iSnapOld":     a.iSnapOld,
		"resurrected":  a.resurrected,
		"lostIngest":   a.lostIngest,
	}, nil
}

// --- the redaction pass ------------------------------------------------

// RedactRead snapshots the page through the real page-read API.
func (a *wikiRedactAdapter) RedactRead() error {
	if !a.gate.pass(a.rPhase == "idle") {
		return nil
	}
	var pg struct {
		Body string `json:"body"`
	}
	if err := a.api(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(wrrPage), nil, &pg); err != nil {
		return err
	}
	a.rPhase, a.rSnapOld, a.rSnapApp = "reading", a.diskOld(pg.Body), a.diskAppended(pg.Body)
	return nil
}

// RedactWrite writes the whole page back from the stale snapshot with
// every secret it saw stripped, through the real write API: a genuine
// read-modify-write with no base/lock, exactly what a lockless scrub
// pass would do.
func (a *wikiRedactAdapter) RedactWrite() error {
	if !a.gate.pass(a.rPhase == "reading") {
		return nil
	}
	a.rPhase = "idle"
	snapApp := a.rSnapApp
	if a.bug == "fresh-read" {
		// The wrong adapter: re-reads instead of writing from the
		// snapshot, so it never loses an ingest write in flight.
		body, err := a.body()
		if err != nil {
			return err
		}
		snapApp = a.diskAppended(body)
	}
	cur, err := a.body()
	if err != nil {
		return err
	}
	if a.diskAppended(cur) && !a.rSnapApp {
		a.lostIngest = true
	}
	newBody := wrrBody(false, snapApp)
	return a.api(http.MethodPut, "/api/wiki/page", map[string]any{"path": wrrPage, "body": newBody}, nil)
}

// --- the ingest tick -----------------------------------------------------

// IngestRead starts the real `bough wiki run` tick (a headless session
// held by llm-control), waits for it to take its turn, and snapshots the
// secret's presence at that moment: what the run's agent would see if it
// read the page now, before it writes anything back.
func (a *wikiRedactAdapter) IngestRead() error {
	if !a.gate.pass(a.iPhase == "idle") {
		return nil
	}
	a.turn++
	id := fmt.Sprintf("wrr-%04d", a.turn)
	if err := writeSession(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"), time.Now().Add(-time.Hour), "session "+id); err != nil {
		return err
	}
	a.pends = append(a.pends, id)

	name := fmt.Sprintf("wrr%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "ingest done"})
	logf, err := os.OpenFile(filepath.Join(a.wiki, "ingest.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	cmd := exec.Command(a.s.Bin(), "wiki", "run")
	cmd.Dir = a.wiki
	cmd.Stdout, cmd.Stderr = logf, logf
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if k == "HOME" || k == "BOUGH_WEB_ADDR" || k == "BOUGH_BIN" || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		cmd.Env = append(cmd.Env, kv)
	}
	cmd.Env = append(append(cmd.Env, "HOME="+a.s.Home, "BOUGH_WEB_ADDR=127.0.0.1:0"), wrrGitEnv()...)
	if err := cmd.Start(); err != nil {
		logf.Close()
		return err
	}
	done := make(chan struct{})
	go func() { cmd.Wait(); logf.Close(); close(done) }()
	alive := func() bool {
		select {
		case <-done:
			return false
		default:
			return true
		}
	}
	taken := filepath.Join(a.dir, name+".taken")
	if err := poll("the ingest run to reach the lock", func() (bool, error) {
		return wrrExists(taken), nil
	}); err != nil {
		return err
	}
	body, err := a.body()
	if err != nil {
		return err
	}
	a.run = &wrrRun{turn: name, alive: alive}
	a.iPhase, a.iSnapOld = "reading", a.diskOld(body)
	return nil
}

// IngestWrite is the run's agent writing the whole page back from the
// stale snapshot plus its freshly appended (already-redacted) excerpt,
// then the parked "ingest ..." commit landing and the run exiting.
func (a *wikiRedactAdapter) IngestWrite() error {
	if !a.gate.pass(a.iPhase == "reading") {
		return nil
	}
	a.iPhase = "idle"
	r := a.run
	body, err := a.body()
	if err != nil {
		return err
	}
	if a.iSnapOld && !a.diskOld(body) {
		a.resurrected = true
	}
	if err := os.WriteFile(a.path(wrrPage), []byte(wrrBody(a.iSnapOld, true)), 0o644); err != nil {
		return err
	}
	logf, err := os.OpenFile(a.path("log.md"), os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	fmt.Fprintf(logf, "\n## [%s] ingest | %s#3 | ingested | -\n", time.Now().Format("2006-01-02"), a.pends[len(a.pends)-1])
	logf.Close()
	control.Release(a.t, a.dir, r.turn)
	if err := poll("the ingest run's commit to start", func() (bool, error) {
		if wrrExists(filepath.Join(a.mark, "committing")) {
			return true, nil
		}
		if !r.alive() {
			return false, fmt.Errorf("the ingest run exited without committing; ingest.log:\n%s", a.log())
		}
		return false, nil
	}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(a.mark, "release"), nil, 0o644); err != nil {
		return err
	}
	if err := poll("the ingest run to commit and exit", func() (bool, error) {
		return !r.alive() && !wrrLockHeld(a.wiki), nil
	}); err != nil {
		return err
	}
	a.run = nil
	return nil
}

func (a *wikiRedactAdapter) log() string {
	b, _ := os.ReadFile(filepath.Join(a.wiki, "ingest.log"))
	return string(b)
}

// wrrLockHeld probes .ingest.lock the way the next tick would: a
// non-blocking flock, dropped at once when it succeeds.
func wrrLockHeld(dir string) bool {
	f, err := os.OpenFile(filepath.Join(dir, ".ingest.lock"), os.O_RDWR, 0)
	if err != nil {
		return false
	}
	defer f.Close()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		return true
	}
	syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
	return false
}

// --- HTTP -----------------------------------------------------------

// api is one request to the serve's HTTP API; a non-2xx answer is a
// *servetest.APIError.
func (a *wikiRedactAdapter) api(method, path string, body, out any) error {
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
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
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

// wrrAct wraps an action so a walk counts what it really took.
func wrrAct(name string, f func(*wikiRedactAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*wikiRedactAdapter)
		err := f(a)
		if !a.gate.off {
			a.ran[name]++
		}
		return nil, err
	}
}

var wikiRedactActions = map[string]map[string]fmbt.ActionFunc{"Page": {
	"RedactRead":  wrrAct("RedactRead", (*wikiRedactAdapter).RedactRead),
	"RedactWrite": wrrAct("RedactWrite", (*wikiRedactAdapter).RedactWrite),
	"IngestRead":  wrrAct("IngestRead", (*wikiRedactAdapter).IngestRead),
	"IngestWrite": wrrAct("IngestWrite", (*wikiRedactAdapter).IngestWrite),
}}

func wikiRedactOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// wikiRedactHistory projects the ingest run's headless transcript: its
// input is IngestRead reaching the lock, and its done is IngestWrite's
// page rewrite and commit. The redaction pass leaves no transcript.
func wikiRedactHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Page#0.iPhase": "idle"}}}
	for _, e := range entries {
		switch e.Kind {
		case "input":
			steps = append(steps, tracecheck.Step{Action: "Page#0.IngestRead", State: map[string]any{"Page#0.iPhase": "reading"}})
		case "done":
			steps = append(steps, tracecheck.Step{Action: "Page#0.IngestWrite", State: map[string]any{"Page#0.iPhase": "idle"}})
		}
	}
	return steps
}

func init() { historyProjections["wiki_redact_vs_ingest_race"] = wikiRedactHistory }

func (a *wikiRedactAdapter) runSessions(t *testing.T) []string {
	infos, err := history.List(filepath.Join(a.s.Home, ".bough", "history"))
	if err != nil {
		t.Fatal(err)
	}
	var ids []string
	for _, in := range infos {
		if in.Cwd != "" && sameDir(in.Cwd, a.wiki) {
			ids = append(ids, in.ID)
		}
	}
	return ids
}

// walkAll walks every path over the checked-in graph for cover and
// returns a line per path that failed (only the first with stopAtFirst).
func (a *wikiRedactAdapter) walkAll(t *testing.T, cover tracecheck.Cover, stopAtFirst bool) []string {
	t.Helper()
	b, err := pathsJSONCover("wiki_redact_vs_ingest_race", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	var fails []string
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace[1:] {
				names = append(names, strings.TrimPrefix(s.Action, "Page#0."))
			}
			fails = append(fails, fmt.Sprintf("path %d [%s]: %v", i, strings.Join(names, " "), err))
			if stopAtFirst {
				break
			}
		}
	}
	t.Logf("wiki_redact_vs_ingest_race: %d paths, %d failed; actions taken: %v", len(doc.Paths), len(fails), a.ran)
	return fails
}

// walk replays one path: every action must pass its gate, and after
// every step the state read must be the path's.
func (a *wikiRedactAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("Cleanup: %w", cerr)
		}
	}()
	for i, step := range trace {
		name := strings.TrimPrefix(step.Action, "Page#0.")
		if i == 0 {
			err = a.Init()
		} else if f := wikiRedactActions["Page"][name]; f == nil {
			err = fmt.Errorf("no action %q", name)
		} else {
			_, err = f(a, nil)
			if err == nil && a.gate.off {
				err = errors.New("the adapter reads it as disabled")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w", i, name, err)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		raw, _ := json.Marshal(got)
		var have map[string]any
		if err := json.Unmarshal(raw, &have); err != nil {
			return err
		}
		var diff []string
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Page#0.")
			if ok && !reflect.DeepEqual(have[field], want) {
				diff = append(diff, fmt.Sprintf("%s: want %v, got %v", field, want, have[field]))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s): %s", i, name, strings.Join(diff, "; "))
		}
	}
	return nil
}

// TestWikiRedactVsIngestRace is the fizzbee-mbt random run; it is part
// of the exhaustive MODEL_COVER=transitions run (see runMBT).
func TestWikiRedactVsIngestRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRedactAdapter(t)
	if err := runMBT(t, "wiki_redact_vs_ingest_race", a, wikiRedactActions, wikiRedactOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// TestWikiRedactVsIngestRacePaths walks the generated paths (every
// state, or every transition under MODEL_COVER=transitions) against a
// real serve, then trace-checks the transcript of every ingest run they
// started.
func TestWikiRedactVsIngestRacePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRedactAdapter(t)
	for _, f := range a.walkAll(t, envCover(), false) {
		t.Error(f)
	}
	g, err := tracecheck.Load(fizzCheck(t, "wiki_redact_vs_ingest_race"))
	if err != nil {
		t.Fatal(err)
	}
	ids := a.runSessions(t)
	if len(ids) == 0 {
		t.Fatal("the walks started no ingest run")
	}
	for _, id := range ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), wikiRedactHistory)
	}
	t.Logf("wiki_redact_vs_ingest_race: trace-checked %d runs", len(ids))
}

// The walk proves nothing unless an adapter that hides the race fails
// it: here RedactWrite re-reads the page instead of writing from its
// stale snapshot, so a redact write that lands after an ingest write
// appended its excerpt no longer wipes that excerpt off disk, and
// diskAppended stops matching the model's rSnapApp-derived diskAppended
// on the path that interleaves IngestWrite between RedactRead and
// RedactWrite.
func TestWikiRedactVsIngestRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRedactAdapter(t)
	a.bug = "fresh-read"
	fails := a.walkAll(t, tracecheck.CoverStates, true)
	if len(fails) == 0 {
		t.Fatal("every path passed with a RedactWrite that re-reads instead of using its snapshot; the walk is not checking state")
	}
	t.Log(fails[0])
}

func TestWikiRedactVsIngestRaceHistoryProjection(t *testing.T) {
	g, err := tracecheck.Load(fizzCheck(t, "wiki_redact_vs_ingest_race"))
	if err != nil {
		t.Fatal(err)
	}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": "/llm-wiki ingest s1"}}
	done := history.Entry{Kind: "done"}
	if v := g.Check(wikiRedactHistory([]history.Entry{input, done})); v != nil {
		t.Errorf("ingest: %v", v)
	}
	if v := g.Check(wikiRedactHistory([]history.Entry{done})); v == nil {
		t.Error("a run that committed without ever taking the lock passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}
