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
	"reflect"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/wiki"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/wiki_run_feedback.fizz against a real serve: Index's and
// Activity's Ingest now, Review's per-session Ingest and Me's Refresh
// meeting a scheduler tick or a running brief at the wiki's one lock.
//
// Every process is the real one: POST /api/wiki/ingest and POST
// /api/me/refresh spawn `bough wiki run` / `bough wiki brief`, the tick
// is a `bough wiki run` the adapter starts the way launchd would, and a
// holder of the lock runs a headless session whose model is llm-control,
// so it holds the lock until the adapter releases its turn.
//
// The spec's spawn is the window between the POST's answer and the
// process reaching .ingest.lock, which the real process crosses in
// milliseconds. The adapter holds it there: serve runs with a `git` on
// its PATH that parks `git -C <wiki> init` (ensureWiki's step just
// before the lock, taken on every run because the wiki never gets a
// .git) until the adapter lets that pid go. Every other git call is the
// real git.
//
// There is one session to ingest, rf-session. SessionFinishes appends a
// new exchange to it rather than writing another: the spec's `pending`
// and Review's listed row are the one waiting session, and a second id
// would make an --only run of the first one idle where the spec says it
// runs (see the flow's notes).

const rfSession = "rf-session"

// rfFakeGit parks ensureWiki's `git -C <...>/.bough/wiki init` until
// the adapter writes <gates>/<pid>.go, pid being the wiki process; it
// gives up when that process is gone (killed at the gate by Cleanup).
const rfFakeGit = `#!/bin/sh
case "$1 $2 $3" in
"-C "*/.bough/wiki" init")
  : > "GATES/$PPID.waiting"
  while [ ! -e "GATES/$PPID.go" ] && kill -0 "$PPID" 2>/dev/null; do sleep 0.02; done
  rm -f "GATES/$PPID.go" "GATES/$PPID.waiting"
  exit 0 ;;
esac
exec REALGIT "$@"
`

// rfRun is the process holding the lock: its kind, its held turn, pid.
type rfRun struct {
	kind string // ingest | brief
	turn string
	pid  int
}

type wikiRunFeedbackAdapter struct {
	t      *testing.T
	s      *servetest.Server
	dir    string // llm-control's queue
	wiki   string // HOME/.bough/wiki
	gates  string // the fake git's gate files
	bin    string // the fake git's directory, first on serve's PATH
	traces string // ingest and brief transcripts of finished walks
	gate   gate

	// The page.
	screen                string
	seenPending, seenBusy bool
	note                  string
	ingestErr             bool
	listed                bool
	row                   string
	refreshing            bool

	// The world.
	spawn    string // none | ingest | session | brief: parked at the gate
	spawnPid int
	fate     string
	held     *rfRun
	seq      int64 // rf-session's last entry
	turn     int

	// bug is a deliberate wiring bug the CatchesWrongAdapter test sets:
	// "note-fresh" picks Activity's note from a read taken on the click
	// instead of the page's last one.
	bug string
}

func newWikiRunFeedbackAdapter(t *testing.T) *wikiRunFeedbackAdapter {
	real, err := exec.LookPath("git")
	if err != nil {
		t.Skip("no git on PATH")
	}
	bin, gates, traces := t.TempDir(), t.TempDir(), t.TempDir()
	script := strings.NewReplacer("GATES", gates, "REALGIT", real).Replace(rfFakeGit)
	if err := os.WriteFile(filepath.Join(bin, "git"), []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	path := "PATH=" + bin + string(os.PathListSeparator) + os.Getenv("PATH")
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: []string{path}})
	a := &wikiRunFeedbackAdapter{t: t, s: s, dir: control.Dir(s.Home), wiki: filepath.Join(s.Home, ".bough", "wiki"), gates: gates, bin: bin, traces: traces}
	t.Cleanup(func() {
		// A walk that failed mid-way may leave a process parked or a
		// turn held: nothing of the serve's may outlive the test.
		if err := a.Cleanup(); err != nil {
			t.Logf("wiki_run_feedback cleanup: %v", err)
		}
	})
	return a
}

func (a *wikiRunFeedbackAdapter) hist(id string) string {
	return filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")
}

// Init: the log with a baseline two days back, rf-session finished an
// hour ago and waiting, a profile (Refresh needs one) and today's brief
// fresh, so no `wiki run` decides a brief is due and takes a turn the
// walk did not queue. The last walk's ingest and brief transcripts are
// moved aside for the trace check, so Activity lists only this walk's.
func (a *wikiRunFeedbackAdapter) Init() error {
	a.gate.reset()
	a.screen, a.note, a.ingestErr, a.listed, a.row, a.refreshing = "index", "", false, false, "", false
	a.spawn, a.spawnPid, a.fate, a.held = "none", 0, "", nil
	for _, id := range a.ingestSessions() {
		if err := os.Rename(a.hist(id), filepath.Join(a.traces, id+".jsonl")); err != nil {
			return err
		}
	}
	files := map[string]string{
		"log.md":         "# Wiki log\n\n<!-- baseline: " + time.Now().Add(-48*time.Hour).UTC().Format(time.RFC3339) + " -->\n",
		wiki.ProfilePath: "# Me\n\nI own acme/web.\n\n## Mine\n\n## Not mine\n\n## Watch\n",
	}
	for rel, body := range files {
		p := filepath.Join(a.wiki, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			return err
		}
	}
	if err := a.freshenBrief(); err != nil {
		return err
	}
	a.seq = 0
	if err := os.Remove(a.hist(rfSession)); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := a.finish(); err != nil {
		return err
	}
	return a.read()
}

// freshenBrief keeps today's brief younger than briefEvery: Run writes
// a brief first when one is due, and that would ask llm-control for a
// turn nobody queued.
func (a *wikiRunFeedbackAdapter) freshenBrief() error {
	p := filepath.Join(a.wiki, filepath.FromSlash(wiki.BriefPath(time.Now())))
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	if _, err := os.Stat(p); err != nil {
		if err := os.WriteFile(p, []byte("# Today\n\nNothing new.\n"), 0o644); err != nil {
			return err
		}
	}
	now := time.Now()
	return os.Chtimes(p, now, now)
}

// finish appends one exchange to rf-session and dates the file an hour
// back, so FindPending sees it quiet and not yet ingested past seq.
func (a *wikiRunFeedbackAdapter) finish() error {
	p := a.hist(rfSession)
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	at := time.Now().Add(-time.Hour)
	var buf bytes.Buffer
	for _, e := range []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "how the demo works"}},
		{Kind: "assistant", Data: map[string]any{"text": "it works like this"}},
		{Kind: "done", Data: map[string]any{}},
	} {
		a.seq++
		e.Seq, e.At, e.Parent = a.seq, at, a.seq-1
		b, err := json.Marshal(e)
		if err != nil {
			return err
		}
		buf.Write(append(b, '\n'))
	}
	f, err := os.OpenFile(p, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	if _, err := f.Write(buf.Bytes()); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Chtimes(p, at, at)
}

// Cleanup kills a process still parked at the gate (it holds nothing)
// and lets a held run finish, so the next walk starts with the lock
// free and no brief counted.
func (a *wikiRunFeedbackAdapter) Cleanup() error {
	if a.spawn != "none" {
		syscall.Kill(a.spawnPid, syscall.SIGKILL)
		// The fake git notices within its 20 ms tick and removes its
		// file; until then the next walk could take it for its own.
		if err := poll("the parked process to go", func() (bool, error) { return !a.parked(a.spawnPid) && !gateFile(a.gates, a.spawnPid), nil }); err != nil {
			return err
		}
		a.spawn, a.spawnPid = "none", 0
	}
	if a.held != nil {
		if err := a.end(); err != nil {
			return err
		}
	}
	return a.waitMeBusy()
}

func (a *wikiRunFeedbackAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Runs", Index: 0}: a}, nil
}

// lock is who holds .ingest.lock, read off Activity's running row (an
// ingest's command is "/llm-wiki ingest …", a brief's "/llm-wiki
// brief") and checked against the lock file itself.
func (a *wikiRunFeedbackAdapter) lock(act wiki.Activity) (string, error) {
	var running []wiki.IngestRun
	for _, r := range act.Runs {
		if r.Running {
			running = append(running, r)
		}
	}
	lock := "free"
	switch {
	case len(running) > 1:
		return "", fmt.Errorf("Activity shows %d running runs; one lock admits one", len(running))
	case len(running) == 1 && strings.HasPrefix(running[0].Command, "/llm-wiki brief"):
		lock = "brief"
	case len(running) == 1:
		lock = "ingest"
	}
	if held := wikiLockHeld(a.wiki); held != (lock != "free") {
		return "", fmt.Errorf("Activity says the lock is %s, the lock file says held=%v", lock, held)
	}
	return lock, nil
}

func (a *wikiRunFeedbackAdapter) activity() (wiki.Activity, error) {
	var act wiki.Activity
	err := a.api(http.MethodGet, "/api/wiki/activity", nil, &act)
	return act, err
}

// store is the lock and whether rf-session waits, from Activity.
func (a *wikiRunFeedbackAdapter) store() (lock string, pending bool, err error) {
	act, err := a.activity()
	if err != nil {
		return "", false, err
	}
	lock, err = a.lock(act)
	return lock, act.Pending > 0, err
}

func (a *wikiRunFeedbackAdapter) GetState() (map[string]any, error) {
	lock, pending, err := a.store()
	if err != nil {
		return nil, err
	}
	// spawn is the adapter's, but only while the process really waits
	// at the gate: one that got past it is not "on the way".
	spawn := a.spawn
	if spawn != "none" && !a.parked(a.spawnPid) {
		spawn = "none"
	}
	return map[string]any{
		"screen": a.screen, "lock": lock, "pending": pending, "spawn": spawn, "fate": a.fate,
		"seenPending": a.seenPending, "seenBusy": a.seenBusy, "note": a.note,
		"ingestErr": a.ingestErr, "listed": a.listed, "row": a.row, "refreshing": a.refreshing,
	}, nil
}

// --- the page's reads -------------------------------------------------

type meRunning struct {
	Running bool `json:"running"`
}

// observe is what the screen's own endpoint says now: the waiting count
// and the run it shows (Activity's running row, Me's "Brief running").
func (a *wikiRunFeedbackAdapter) observe() (pending, busy bool, err error) {
	switch a.screen {
	case "me":
		var m meRunning
		err = a.api(http.MethodGet, "/api/me", nil, &m)
		return false, m.Running, err
	case "activity":
		act, err := a.activity()
		if err != nil {
			return false, false, err
		}
		return act.Pending > 0, slices.ContainsFunc(act.Runs, func(r wiki.IngestRun) bool { return r.Running }), nil
	case "index":
		var ix wiki.Index
		err = a.api(http.MethodGet, "/api/wiki", nil, &ix)
		return ix.Health.Pending > 0, false, err
	}
	return false, false, nil
}

// read is the current screen reading the store.
func (a *wikiRunFeedbackAdapter) read() error {
	p, b, err := a.observe()
	a.seenPending, a.seenBusy = p, b
	return err
}

// fresh: the screen's last read is what its endpoint says now.
func (a *wikiRunFeedbackAdapter) fresh() bool {
	p, b, err := a.observe()
	if err != nil {
		a.t.Logf("wiki_run_feedback: reading for a gate: %v", err)
	}
	return err == nil && p == a.seenPending && b == a.seenBusy
}

// --- the person -------------------------------------------------------

// Go moves to the screen the runner chose. Review's list and row and
// Me's spinner are their views'; the note and the alert are WikiPage's
// and go only when the view leaves the wiki.
func (a *wikiRunFeedbackAdapter) Go(args []fmbt.Arg) error {
	to, _ := argOf(args, "to").(string)
	if !a.gate.pass(a.spawn == "none" && to != "" && to != a.screen && a.fresh()) {
		return nil
	}
	a.listed, a.row, a.refreshing = false, "", false
	if to == "me" {
		a.note, a.ingestErr = "", false
	}
	a.screen = to
	if err := a.read(); err != nil {
		return err
	}
	if to == "review" {
		var rv wiki.Review
		if err := a.api(http.MethodGet, "/api/wiki/review", nil, &rv); err != nil {
			return err
		}
		a.listed = slices.ContainsFunc(rv.Pending, func(p wiki.PendingRef) bool { return p.ID == rfSession })
	}
	return nil
}

// IngestIndex is the Indexing state's Ingest now: the POST, then the
// index's reload.
func (a *wikiRunFeedbackAdapter) IngestIndex() error {
	if !a.gate.pass(a.screen == "index" && a.spawn == "none") {
		return nil
	}
	a.ingestErr = false
	if err := a.start("ingest", "/api/wiki/ingest", nil); err != nil {
		return err
	}
	return a.read()
}

// IngestIndexFails is the same click with ingest.log a directory, so
// spawnWiki cannot open it and the POST fails before any process.
func (a *wikiRunFeedbackAdapter) IngestIndexFails() error {
	if !a.gate.pass(a.screen == "index" && a.spawn == "none") {
		return nil
	}
	logf := filepath.Join(a.wiki, "ingest.log")
	aside := logf + ".aside"
	if err := os.Rename(logf, aside); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := os.Mkdir(logf, 0o755); err != nil {
		return err
	}
	err := a.api(http.MethodPost, "/api/wiki/ingest", nil, nil)
	if rerr := os.Remove(logf); rerr != nil {
		return rerr
	}
	if rerr := os.Rename(aside, logf); rerr != nil && !errors.Is(rerr, os.ErrNotExist) {
		return rerr
	}
	var e *servetest.APIError
	if !errors.As(err, &e) {
		return fmt.Errorf("an ingest POST with ingest.log unopenable answered %v, not an error", err)
	}
	a.ingestErr = true
	return nil
}

// IngestNow is Activity's button: the note is picked from the read the
// page already had, then Activity reads again.
func (a *wikiRunFeedbackAdapter) IngestNow() error {
	if !a.gate.pass(a.screen == "activity" && a.spawn == "none") {
		return nil
	}
	seen := a.seenPending
	if a.bug == "note-fresh" {
		p, _, err := a.observe()
		if err != nil {
			return err
		}
		seen = p
	}
	if err := a.start("ingest", "/api/wiki/ingest", nil); err != nil {
		return err
	}
	a.note = "nothing"
	if seen {
		a.note = "started"
	}
	return a.read()
}

// IngestSession is Review's row: POST /api/wiki/ingest {session}.
func (a *wikiRunFeedbackAdapter) IngestSession() error {
	if !a.gate.pass(a.screen == "review" && a.listed && a.row == "" && a.spawn == "none") {
		return nil
	}
	if err := a.start("session", "/api/wiki/ingest", map[string]string{"session": rfSession}); err != nil {
		return err
	}
	a.row = "started"
	return nil
}

// Refresh is Me's button: the POST, the reload and the 20 s spinner.
func (a *wikiRunFeedbackAdapter) Refresh() error {
	if !a.gate.pass(a.screen == "me" && !a.refreshing && a.spawn == "none") {
		return nil
	}
	if err := a.start("brief", "/api/me/refresh", nil); err != nil {
		return err
	}
	a.refreshing = true
	return a.read()
}

// start sends the POST and waits for the process it spawned to park at
// the gate: the POST has answered, the lock is still ahead of it.
func (a *wikiRunFeedbackAdapter) start(kind, path string, body any) error {
	if err := a.freshenBrief(); err != nil {
		return err
	}
	if err := a.api(http.MethodPost, path, body, nil); err != nil {
		return err
	}
	pid, err := a.waitParked(0)
	if err != nil {
		return err
	}
	a.spawn, a.spawnPid, a.fate = kind, pid, ""
	return nil
}

// --- the processes ----------------------------------------------------

// Reach lets the parked process go to the lock and waits to see what it
// did there: take the turn queued for it (it ran), or exit. An ingest
// that exits dropped when the lock was held and was idle when it was
// free; a brief that exits must have said so in ingest.log.
func (a *wikiRunFeedbackAdapter) Reach() error {
	if !a.gate.pass(a.spawn != "none") {
		return nil
	}
	kind, pid := a.spawn, a.spawnPid
	lock, _, err := a.store()
	if err != nil {
		return err
	}
	refusals := strings.Count(a.log(), "already running")
	name := a.queue()
	if err := os.WriteFile(filepath.Join(a.gates, strconv.Itoa(pid)+".go"), nil, 0o644); err != nil {
		return err
	}
	a.spawn, a.spawnPid = "none", 0
	ran, err := a.await(name, pid)
	if err != nil {
		return err
	}
	switch {
	case ran:
		a.fate = "ran"
		held := "ingest"
		if kind == "brief" {
			held = "brief"
		}
		a.held = &rfRun{kind: held, turn: name, pid: pid}
		if err := a.waitLock(held); err != nil {
			return err
		}
	case lock == "free":
		a.fate = "idle"
	case kind == "brief":
		a.fate = "refused"
		if strings.Count(a.log(), "already running") <= refusals {
			return fmt.Errorf("a brief that met the lock held by %s exited without saying so; ingest.log:\n%s", lock, a.log())
		}
	default:
		a.fate = "dropped"
	}
	return a.waitMeBusy()
}

// SchedulerTick is launchd's tick: `bough wiki run` as the plist runs
// it, let through the gate at once, holding the lock with its turn.
func (a *wikiRunFeedbackAdapter) SchedulerTick() error {
	lock, pending, err := a.store()
	if err != nil {
		return err
	}
	if !a.gate.pass(lock == "free" && pending) {
		return nil
	}
	if err := a.freshenBrief(); err != nil {
		return err
	}
	name := a.queue()
	cmd := exec.Command(a.s.Bin(), "wiki", "run")
	cmd.Dir = a.s.Home
	cmd.Env = a.tickEnv()
	if err := cmd.Start(); err != nil {
		return err
	}
	go cmd.Wait()
	pid := cmd.Process.Pid
	if _, err := a.waitParked(pid); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(a.gates, strconv.Itoa(pid)+".go"), nil, 0o644); err != nil {
		return err
	}
	ran, err := a.await(name, pid)
	if err != nil {
		return err
	}
	if !ran {
		return fmt.Errorf("the tick found the lock free and a session waiting but did not run; ingest.log:\n%s", a.log())
	}
	a.held = &rfRun{kind: "ingest", turn: name, pid: pid}
	return a.waitLock("ingest")
}

// tickEnv is serve's environment for a process the adapter starts: its
// HOME, the fake git first on PATH, no provider keys.
func (a *wikiRunFeedbackAdapter) tickEnv() []string {
	var env []string
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if k == "HOME" || k == "PATH" || k == "BOUGH_WEB_ADDR" || k == "BOUGH_BIN" || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		env = append(env, kv)
	}
	return append(env, "HOME="+a.s.Home, "BOUGH_WEB_ADDR=127.0.0.1:0",
		"PATH="+a.bin+string(os.PathListSeparator)+os.Getenv("PATH"))
}

// SessionFinishes: rf-session has a new exchange, quiet, not ingested.
func (a *wikiRunFeedbackAdapter) SessionFinishes() error {
	_, pending, err := a.store()
	if err != nil {
		return err
	}
	if !a.gate.pass(!pending) {
		return nil
	}
	return a.finish()
}

// RunEnds is the ingest agent's work while its turn is held: "No
// material" for every session the run named, in log.md, then the turn
// ends and the run exits.
func (a *wikiRunFeedbackAdapter) RunEnds() error {
	lock, _, err := a.store()
	if err != nil {
		return err
	}
	if !a.gate.pass(lock == "ingest" && a.held != nil) {
		return nil
	}
	act, err := a.activity()
	if err != nil {
		return err
	}
	var ids []string
	for _, r := range act.Runs {
		if r.Running {
			ids = strings.Fields(strings.TrimPrefix(r.Command, "/llm-wiki ingest"))
		}
	}
	f, err := os.OpenFile(filepath.Join(a.wiki, "log.md"), os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	for _, id := range ids {
		fmt.Fprintf(f, "\n## [%s] ingest | %s#%d | No material | -\n", time.Now().Format("2006-01-02"), id, a.seq)
	}
	if err := f.Close(); err != nil {
		return err
	}
	return a.end()
}

func (a *wikiRunFeedbackAdapter) BriefEnds() error {
	lock, _, err := a.store()
	if err != nil {
		return err
	}
	if !a.gate.pass(lock == "brief" && a.held != nil) {
		return nil
	}
	return a.end()
}

// end lets the held run's turn finish and waits for it to exit, for
// Activity to stop showing it and Me to stop counting a brief.
func (a *wikiRunFeedbackAdapter) end() error {
	r := a.held
	a.held = nil
	control.Release(a.t, a.dir, r.turn)
	if err := poll("the run holding the lock to exit", func() (bool, error) { return !procAlive(r.pid), nil }); err != nil {
		return err
	}
	if err := a.waitLock("free"); err != nil {
		return err
	}
	return a.waitMeBusy()
}

// --- the page's timers ------------------------------------------------

func (a *wikiRunFeedbackAdapter) Settle() error {
	if !a.gate.pass(a.screen == "me" && a.refreshing) {
		return nil
	}
	a.refreshing = false
	return a.read()
}

func (a *wikiRunFeedbackAdapter) pollScreen(screen string) error {
	if !a.gate.pass(a.screen == screen && !a.fresh()) {
		return nil
	}
	return a.read()
}

func (a *wikiRunFeedbackAdapter) ActivityPoll() error { return a.pollScreen("activity") }
func (a *wikiRunFeedbackAdapter) IndexPoll() error    { return a.pollScreen("index") }
func (a *wikiRunFeedbackAdapter) MePoll() error       { return a.pollScreen("me") }

// --- plumbing ---------------------------------------------------------

// queue puts one held turn in llm-control's queue for whatever reaches
// the model next.
func (a *wikiRunFeedbackAdapter) queue() string {
	a.turn++
	name := fmt.Sprintf("rf%05d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "no material"})
	return name
}

// await waits until the process pid either takes the turn name (true)
// or exits without asking the model (false; the turn is withdrawn).
func (a *wikiRunFeedbackAdapter) await(name string, pid int) (bool, error) {
	taken := filepath.Join(a.dir, name+".taken")
	ran := false
	err := poll("the wiki process past the gate to run or exit", func() (bool, error) {
		if _, err := os.Stat(taken); err == nil {
			ran = true
			return true, nil
		}
		if procAlive(pid) {
			return false, nil
		}
		// Exited: take the turn back, unless it was taken just now.
		if err := os.Remove(filepath.Join(a.dir, name+".json")); errors.Is(err, os.ErrNotExist) {
			return false, nil
		}
		return true, nil
	})
	return ran, err
}

// waitParked waits for a process at the gate: pid, or (0) any one.
func (a *wikiRunFeedbackAdapter) waitParked(pid int) (int, error) {
	got := 0
	err := poll("a wiki process to reach the gate before the lock", func() (bool, error) {
		files, _ := filepath.Glob(filepath.Join(a.gates, "*.waiting"))
		for _, f := range files {
			p, _ := strconv.Atoi(strings.TrimSuffix(filepath.Base(f), ".waiting"))
			if (pid == 0 || p == pid) && procAlive(p) {
				got = p
				return true, nil
			}
		}
		return false, nil
	})
	return got, err
}

func (a *wikiRunFeedbackAdapter) parked(pid int) bool {
	return gateFile(a.gates, pid) && procAlive(pid)
}

func gateFile(gates string, pid int) bool {
	_, err := os.Stat(filepath.Join(gates, strconv.Itoa(pid)+".waiting"))
	return err == nil
}

// waitLock waits for Activity and the lock file to agree on want: a
// holder's session writes its input a moment after it takes the lock.
func (a *wikiRunFeedbackAdapter) waitLock(want string) error {
	last := ""
	err := poll("the lock to be "+want, func() (bool, error) {
		lock, _, err := a.store()
		last = lock
		if err != nil {
			last = err.Error()
		}
		return err == nil && lock == want, nil
	})
	if err != nil {
		return fmt.Errorf("%w (last: %s)", err, last)
	}
	return nil
}

// waitMeBusy waits for GET /api/me's running to be what the spec's
// meBusy is: a brief parked at the gate or holding the lock. serve
// uncounts a brief a moment after it exits.
func (a *wikiRunFeedbackAdapter) waitMeBusy() error {
	want := a.spawn == "brief" || (a.held != nil && a.held.kind == "brief")
	return poll(fmt.Sprintf("Me's running to be %v", want), func() (bool, error) {
		var m meRunning
		err := a.api(http.MethodGet, "/api/me", nil, &m)
		return err == nil && m.Running == want, nil
	})
}

func (a *wikiRunFeedbackAdapter) log() string {
	b, _ := os.ReadFile(filepath.Join(a.wiki, "ingest.log"))
	return string(b)
}

// ingestSessions lists the transcripts started in the wiki directory:
// the ingests and briefs.
func (a *wikiRunFeedbackAdapter) ingestSessions() []string {
	infos, err := history.List(filepath.Join(a.s.Home, ".bough", "history"))
	if err != nil {
		return nil
	}
	var ids []string
	for _, in := range infos {
		if in.Cwd != "" && sameDir(in.Cwd, a.wiki) {
			ids = append(ids, in.ID)
		}
	}
	return ids
}

// wikiLockHeld probes .ingest.lock the way the next run would: a
// non-blocking flock, dropped at once when it succeeds.
func wikiLockHeld(dir string) bool {
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

// procAlive: a reaped or zombie process no longer counts.
func procAlive(pid int) bool {
	out, err := exec.Command("ps", "-o", "stat=", "-p", strconv.Itoa(pid)).Output()
	st := strings.TrimSpace(string(out))
	return err == nil && st != "" && !strings.HasPrefix(st, "Z")
}

func (a *wikiRunFeedbackAdapter) api(method, path string, body, out any) error {
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

var wikiRunFeedbackActions = map[string]map[string]fmbt.ActionFunc{"Runs": {
	"Go": func(m any, args []fmbt.Arg) (any, error) {
		return nil, m.(*wikiRunFeedbackAdapter).Go(args)
	},
	"IngestIndex":      action((*wikiRunFeedbackAdapter).IngestIndex),
	"IngestIndexFails": action((*wikiRunFeedbackAdapter).IngestIndexFails),
	"IngestNow":        action((*wikiRunFeedbackAdapter).IngestNow),
	"IngestSession":    action((*wikiRunFeedbackAdapter).IngestSession),
	"Refresh":          action((*wikiRunFeedbackAdapter).Refresh),
	"Reach":            action((*wikiRunFeedbackAdapter).Reach),
	"SchedulerTick":    action((*wikiRunFeedbackAdapter).SchedulerTick),
	"SessionFinishes":  action((*wikiRunFeedbackAdapter).SessionFinishes),
	"RunEnds":          action((*wikiRunFeedbackAdapter).RunEnds),
	"BriefEnds":        action((*wikiRunFeedbackAdapter).BriefEnds),
	"Settle":           action((*wikiRunFeedbackAdapter).Settle),
	"ActivityPoll":     action((*wikiRunFeedbackAdapter).ActivityPoll),
	"IndexPoll":        action((*wikiRunFeedbackAdapter).IndexPoll),
	"MePoll":           action((*wikiRunFeedbackAdapter).MePoll),
}}

// wikiRunFeedbackHistory reads an ingest's or a brief's transcript as
// the steps that must have led to it. An ingest is a tick taking the
// lock (its input) and the run ending (its done); a brief is Me's
// Refresh reaching a free lock and ending. Who started an ingest is not
// in its transcript; what the check holds it to is one input, then at
// most one done, with the lock held in between.
func wikiRunFeedbackHistory(entries []history.Entry) []tracecheck.Step {
	r := func(s string) string { return "Runs#0." + s }
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Runs#0.lock": "free"}}}
	brief := false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			text, _ := e.Data["text"].(string)
			brief = strings.HasPrefix(strings.TrimSpace(text), "/llm-wiki brief")
			if brief {
				steps = append(steps,
					tracecheck.Step{Action: r("Go"), State: map[string]any{"Runs#0.screen": "me"}},
					tracecheck.Step{Action: r("Refresh"), State: map[string]any{"Runs#0.spawn": "brief"}},
					tracecheck.Step{Action: r("Reach"), State: map[string]any{"Runs#0.lock": "brief", "Runs#0.fate": "ran"}})
			} else {
				steps = append(steps, tracecheck.Step{Action: r("SchedulerTick"), State: map[string]any{"Runs#0.lock": "ingest"}})
			}
		case "done":
			if brief {
				steps = append(steps, tracecheck.Step{Action: r("BriefEnds"), State: map[string]any{"Runs#0.lock": "free"}})
			} else {
				steps = append(steps, tracecheck.Step{Action: r("RunEnds"), State: map[string]any{"Runs#0.lock": "free", "Runs#0.pending": false}})
			}
		}
	}
	return steps
}

func init() { historyProjections["wiki_run_feedback"] = wikiRunFeedbackHistory }

// rfWalks is the walks over testdata/wiki_run_feedback's graph.
func rfWalks(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	b, err := pathsJSONCover("wiki_run_feedback", cover)
	if err != nil {
		t.Fatal(err)
	}
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		t.Fatal(err)
	}
	out := make([][]tracecheck.Step, len(f.Paths))
	for i, p := range f.Paths {
		out[i] = p.Trace
	}
	return out
}

// replay walks one path against the serve: every action must pass its
// gate, and after every step the state read must be the path's.
func (a *wikiRunFeedbackAdapter) replay(tr []tracecheck.Step) (err error) {
	if err := a.Init(); err != nil {
		return fmt.Errorf("Init: %w", err)
	}
	defer func() {
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("Cleanup: %w", cerr)
		}
	}()
	for i, step := range tr {
		name := strings.TrimPrefix(step.Action, "Runs#0.")
		if i > 0 {
			f := wikiRunFeedbackActions["Runs"][name]
			if f == nil {
				return fmt.Errorf("step %d: no action %q", i, name)
			}
			var args []fmbt.Arg
			if name == "Go" {
				args = []fmbt.Arg{{Name: "to", Value: step.State["Runs#0.screen"]}}
			}
			if _, err := f(a, args); err != nil {
				return fmt.Errorf("step %d (%s): %w", i, name, err)
			}
			if a.gate.off {
				return fmt.Errorf("step %d (%s): the adapter reads it as disabled", i, name)
			}
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", i, name, err)
		}
		var diff []string
		for k, want := range step.State {
			field, ok := strings.CutPrefix(k, "Runs#0.")
			if ok && !reflect.DeepEqual(got[field], want) {
				diff = append(diff, fmt.Sprintf("%s: want %v, got %v", field, want, got[field]))
			}
		}
		if len(diff) > 0 {
			slices.Sort(diff)
			return fmt.Errorf("step %d (%s): %s", i, name, strings.Join(diff, "; "))
		}
	}
	return nil
}

func rfNames(tr []tracecheck.Step) string {
	var names []string
	for _, s := range tr[1:] {
		n := strings.TrimPrefix(s.Action, "Runs#0.")
		if n == "Go" {
			n += "(" + fmt.Sprint(s.State["Runs#0.screen"]) + ")"
		}
		names = append(names, n)
	}
	return strings.Join(names, " ")
}

// TestWikiRunFeedbackPaths walks the paths derived from the checked-in
// graph against a real serve, then trace-checks every ingest and brief
// transcript those walks left.
func TestWikiRunFeedbackPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRunFeedbackAdapter(t)
	walks := rfWalks(t, envCover())
	for i, tr := range walks {
		if err := a.replay(tr); err != nil {
			t.Errorf("path %d [%s]: %v", i, rfNames(tr), err)
		}
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "wiki_run_feedback"))
	if err != nil {
		t.Fatal(err)
	}
	files, _ := filepath.Glob(filepath.Join(a.traces, "*.jsonl"))
	for _, id := range a.ingestSessions() {
		files = append(files, a.hist(id))
	}
	if len(files) == 0 {
		t.Fatal("the walks ran no ingest and no brief")
	}
	t.Logf("wiki_run_feedback: %d walks, trace-checking %d runs", len(walks), len(files))
	for _, f := range files {
		entries, err := history.Read(f)
		if err != nil {
			t.Fatal(err)
		}
		checkHistory(t, g, entries, wikiRunFeedbackHistory)
	}
}

// The walk must fail on an adapter that breaks the model: here Activity's
// note is picked from a read taken on the click, which differs from the
// page's only on the transition where an ingest landed between Activity's
// last read and the click. Only the transitions walks that take it run.
func TestWikiRunFeedbackPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRunFeedbackAdapter(t)
	a.bug = "note-fresh"
	var stale [][]tracecheck.Step
	for _, tr := range rfWalks(t, tracecheck.CoverTransitions) {
		for i := 1; i < len(tr); i++ {
			prev := tr[i-1].State
			if tr[i].Action == "Runs#0.IngestNow" && prev["Runs#0.seenPending"] != prev["Runs#0.pending"] {
				stale = append(stale, tr[:i+1])
				break
			}
		}
	}
	if len(stale) == 0 {
		t.Fatal("no walk clicks Ingest now on a stale read")
	}
	for _, tr := range stale[:1] {
		err := a.replay(tr)
		if err == nil {
			t.Fatalf("a walk [%s] passed with the note picked from a fresh read", rfNames(tr))
		}
		t.Log(err)
	}
}
