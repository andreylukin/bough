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
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servepid"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/wiki"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/wiki_edit_vs_ingest_lock.fizz against a real serve: the page
// editor's Save and Review's claim decisions against the ingest and
// brief runs that rewrite the same wiki.
//
// The runs are the real ones: Activity's Ingest now and the Me page's
// Refresh are the serve's POSTs, which spawn `bough wiki run` and `bough
// wiki brief`, and the scheduler's tick is `bough wiki run` started by
// the adapter the way launchd starts it. Each runs a headless session
// whose model is llm-control, so its agent phase is held until the walk
// ends it; what the agent writes with its tools the adapter writes while
// the turn is held. The commit phase is held too: a commit-msg hook in
// the wiki's repo parks a run's "ingest ..." or "brief ..." commit (git
// holds the index while it runs) until the walk lets it land.

const (
	welPage  = "topics/demo/alpha.md"
	welOther = "topics/demo/beta.md" // what a run's agent writes besides P
)

// welPageBody is P as a run's agent writes it: a lede and one uncited
// claim, the flagged one. A hand edit adds a line to the lede, which
// moves the claim's lines without touching its flag.
func welPageBody(version int) string {
	return "# Alpha\n\nThe demo page.\n\n## Facts\n\n" +
		fmt.Sprintf("- The flagged fact, version %d, has no entry behind it.\n", version)
}

// welHook parks a run's commit (subject "ingest ..." or "brief ...")
// with the index held: it records what is staged, marks the phase, and
// waits for the adapter's release. Hand commits pass straight through.
// It gives up after two minutes so a dead walk cannot wedge the wiki.
const welHook = `#!/bin/sh
case "$(head -n 1 "$1")" in
"ingest "*|"brief "*)
  git diff --cached --name-only > "%[1]s/staged"
  : > "%[1]s/committing"
  n=0
  while [ ! -e "%[1]s/release" ] && [ $n -lt 6000 ]; do sleep 0.02; n=$((n+1)); done
  rm -f "%[1]s/release" "%[1]s/committing"
  ;;
esac
exit 0
`

// welRun is the process that holds the wiki lock.
type welRun struct {
	kind  string // "ingest" or "brief"
	turn  string
	phase string // "running" or "committing"
	alive func() bool
}

type wikiEditAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	wiki string // HOME/.bough/wiki
	mark string // the commit hook's handshake dir
	pid  int    // serve's pid: the runs it spawns are its children
	gate gate

	// The page editor and Review, which are the client's.
	editor string
	base   string // the body the editor read
	shown  *wiki.Flag

	// The world.
	spawned string // "", or the action that started a process: IngestNow, RefreshBrief, SchedulerTick
	run     *welRun
	hand    bool // P's last write was a hand edit
	version int
	edits   int
	turn    int
	nsess   int
	pends   []string // pending sessions this walk wrote
	logged  int      // how many of pends log.md records

	// Ghosts: what the adapter saw go wrong, sticky for the walk.
	lost, unrecorded, swepthand, sweptrun bool

	// bug is a deliberate wiring bug a CatchesWrongAdapter test injects:
	// "save-no-base" saves without the body it read, as the editor did.
	bug string

	ran map[string]int
}

func newWikiEditAdapter(t *testing.T) *wikiEditAdapter {
	// The wiki's commits and the adapter's reads must not see the host's
	// git config (a global hooksPath would bypass the hook below).
	s := servetest.Start(t, servetest.Options{Config: controlConfig, Env: welGitEnv()})
	a := &wikiEditAdapter{t: t, s: s, dir: control.Dir(s.Home), wiki: filepath.Join(s.Home, ".bough", "wiki"), mark: filepath.Join(s.Root, "wiki-hook"), ran: map[string]int{}}
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

func welGitEnv() []string {
	return []string{"GIT_CONFIG_GLOBAL=" + os.DevNull, "GIT_CONFIG_NOSYSTEM=1"}
}

// git runs git in the wiki as the adapter: never taking the index lock,
// so a read cannot collide with a parked run commit.
func (a *wikiEditAdapter) git(args ...string) (string, error) {
	cmd := exec.Command("git", append([]string{"--no-optional-locks", "-C", a.wiki}, args...)...)
	cmd.Env = append(os.Environ(), append(welGitEnv(), "HOME="+a.s.Home)...)
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	if err := cmd.Run(); err != nil {
		return out.String(), fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, errb.String())
	}
	return out.String(), nil
}

func (a *wikiEditAdapter) path(rel string) string {
	return filepath.Join(a.wiki, filepath.FromSlash(rel))
}

// Init puts the wiki back as seeded, committed: P at version 0 with its
// flagged claim, the run's other page, a profile (Refresh needs one) and
// a fresh brief for today (so a scheduler run does not brief first), a
// log whose baseline predates the pending sessions, and the hook.
func (a *wikiEditAdapter) Init() error {
	for _, id := range a.pends {
		if err := os.Remove(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl")); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
	}
	a.editor, a.base, a.shown = "closed", "", nil
	a.spawned, a.run, a.hand, a.version, a.pends, a.logged = "", nil, false, 0, nil, 0
	a.lost, a.unrecorded, a.swepthand, a.sweptrun = false, false, false, false
	a.gate.reset()
	if err := os.RemoveAll(a.path("topics/demo")); err != nil {
		return err
	}
	files := map[string]string{
		welPage:                    welPageBody(0),
		welOther:                   "# Beta\n\nWhat the runs touch.\n",
		"index.md":                 "# Wiki index\n\n## demo\n\n- [Alpha](" + welPage + ") — the demo page\n- [Beta](" + welOther + ") — the other one\n",
		"log.md":                   "# Wiki log\n\n<!-- baseline: " + time.Now().Add(-48*time.Hour).UTC().Format(time.RFC3339) + " -->\n",
		".gitignore":               ".ingest.lock\ningest.log\n",
		wiki.ProfilePath:           "# Me\n\nThe demo person.\n",
		wiki.BriefPath(time.Now()): "# " + time.Now().Format("2006-01-02") + "\n\nNothing yet.\n",
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
		if err := os.WriteFile(hook, []byte(fmt.Sprintf(welHook, a.mark)), 0o755); err != nil {
			return err
		}
	}
	for _, f := range []string{"staged", "committing", "release"} {
		os.Remove(filepath.Join(a.mark, f))
	}
	if _, err := a.git("add", "-A"); err != nil {
		return err
	}
	_, err := a.git("-c", "user.name=walk", "-c", "user.email=walk@localhost", "commit", "-q", "--allow-empty", "-m", "walk: start")
	return err
}

// Cleanup ends the run the walk left holding the lock: its agent fails
// (runTimeout), and a commit it parks is let through.
func (a *wikiEditAdapter) Cleanup() error {
	r := a.run
	if r == nil {
		return nil
	}
	a.run = nil
	if r.phase == "running" {
		control.ReleaseWith(a.t, a.dir, r.turn, control.Turn{Mode: "error", Error: "walk over"})
	}
	return poll("the walk's run to exit", func() (bool, error) {
		if exists(filepath.Join(a.mark, "committing")) {
			os.WriteFile(filepath.Join(a.mark, "release"), nil, 0o644)
		}
		return !r.alive() && !wikiLockHeld(a.wiki), nil
	})
}

func exists(p string) bool { _, err := os.Stat(p); return err == nil }

func (a *wikiEditAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Wiki", Index: 0}: a}, nil
}

// page is GET /api/wiki/page for P: its body, or present false on 404.
func (a *wikiEditAdapter) page() (body string, present bool, err error) {
	var pg wiki.Page
	err = a.api(http.MethodGet, "/api/wiki/page?path="+url.QueryEscape(welPage), nil, &pg)
	if status(err) == http.StatusNotFound {
		return "", false, nil
	}
	return pg.Body, err == nil, err
}

// flag is P's claim flag in GET /api/wiki/review, nil when there is none.
func (a *wikiEditAdapter) flag() (*wiki.Flag, error) {
	var rv wiki.Review
	if err := a.api(http.MethodGet, "/api/wiki/review", nil, &rv); err != nil {
		return nil, err
	}
	for _, f := range rv.Flags {
		if f.Kind != "problem" && f.Page == welPage {
			return &f, nil
		}
	}
	return nil, nil
}

// phase is the lock holder's, read off the world: the lock, and the
// hook's mark while a run's commit is parked.
func (a *wikiEditAdapter) phase() string {
	switch {
	case !wikiLockHeld(a.wiki):
		return ""
	case exists(filepath.Join(a.mark, "committing")):
		return "committing"
	}
	return "running"
}

// dirty: files in the tree no commit holds. While a run's commit is
// parked, what it staged is on its way in; only what changed since
// counts.
func (a *wikiEditAdapter) dirty(phase string) (bool, error) {
	out, err := a.git("status", "--porcelain")
	if err != nil {
		return false, err
	}
	for _, l := range strings.Split(out, "\n") {
		if len(l) < 3 {
			continue
		}
		if phase != "committing" || l[1] != ' ' {
			return true, nil
		}
	}
	return false, nil
}

// committed says P on disk is what HEAD holds.
func (a *wikiEditAdapter) committed() bool {
	_, err := a.git("diff", "--quiet", "HEAD", "--", welPage)
	return err == nil
}

// linesAt: the claim Review read is still at the lines it read.
func linesAt(body string, f *wiki.Flag) bool {
	lines := strings.Split(body, "\n")
	return f.Line >= 1 && f.End >= f.Line && f.End <= len(lines) && strings.Join(lines[f.Line-1:f.End], "\n") == f.Raw
}

func (a *wikiEditAdapter) GetState() (map[string]any, error) {
	body, present, err := a.page()
	if err != nil {
		return nil, err
	}
	f, err := a.flag()
	if err != nil {
		return nil, err
	}
	phase := a.phase()
	held := 0
	if phase != "" {
		held = 1
	}
	dirty, err := a.dirty(phase)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"editor":     a.editor,
		"moved":      a.editor != "closed" && (!present || body != a.base),
		"present":    present,
		"flagged":    f != nil,
		"shown":      a.shown != nil,
		"rmoved":     a.shown != nil && (!present || !linesAt(body, a.shown)),
		"spawned":    a.spawned != "",
		"held":       held,
		"run":        phase,
		"dirty":      dirty,
		"handdirty":  a.hand && present && !a.committed(),
		"lost":       a.lost,
		"unrecorded": a.unrecorded,
		"swepthand":  a.swepthand,
		"sweptrun":   a.sweptrun,
	}, nil
}

func (a *wikiEditAdapter) phaseView() string {
	if a.run == nil {
		return ""
	}
	return a.run.phase
}

func (a *wikiEditAdapter) present() bool {
	_, ok, err := a.page()
	if err != nil {
		a.t.Logf("wiki_edit_vs_ingest_lock: reading P for a gate: %v", err)
	}
	return ok
}

// --- the page editor ------------------------------------------------

func (a *wikiEditAdapter) Edit() error {
	if !a.gate.pass(a.editor == "closed" && a.present()) {
		return nil
	}
	return a.read()
}

// read is the editor loading P: what it holds is what it read.
func (a *wikiEditAdapter) read() error {
	body, ok, err := a.page()
	if err != nil {
		return err
	}
	if !ok {
		return fmt.Errorf("P is gone")
	}
	a.editor, a.base = "open", body
	return nil
}

func (a *wikiEditAdapter) Cancel() error {
	if !a.gate.pass(a.editor != "closed") {
		return nil
	}
	a.editor, a.base = "closed", ""
	return nil
}

func (a *wikiEditAdapter) Reload() error {
	if !a.gate.pass(a.editor == "stale" && a.present()) {
		return nil
	}
	return a.read()
}

// handEdit is what the person types: one more line in the lede, which
// moves every line under it.
func (a *wikiEditAdapter) handEdit(body string) string {
	a.edits++
	lines := strings.Split(body, "\n")
	i := 3 // under "The demo page." and the hand edits already there
	for i < len(lines) && strings.TrimSpace(lines[i]) != "" {
		i++
	}
	return strings.Join(slices.Insert(lines, i, fmt.Sprintf("Hand edit %d.", a.edits)), "\n")
}

// Save is PUT /api/wiki/page with the body the editor read as its base.
func (a *wikiEditAdapter) Save() error {
	if !a.gate.pass(a.editor == "open" && a.phaseView() != "committing") {
		return nil
	}
	now, _, err := a.page()
	if err != nil {
		return err
	}
	req := map[string]any{"path": welPage, "body": a.handEdit(a.base), "base": a.base}
	if a.bug == "save-no-base" {
		delete(req, "base")
	}
	err = a.api(http.MethodPut, "/api/wiki/page", req, nil)
	switch status(err) {
	case http.StatusNotFound:
		a.editor = "gone"
		return nil
	case http.StatusConflict:
		a.editor = "stale"
		return nil
	case 0:
	default:
		return err
	}
	a.lost = a.lost || now != a.base
	a.editor, a.base, a.hand = "closed", "", true
	return a.checkHandCommit("edit " + welPage)
}

// checkHandCommit reads the commit a hand write just made: it must be
// HEAD, be named subject, hold P as written, and hold nothing else.
func (a *wikiEditAdapter) checkHandCommit(subject string) error {
	subj, err := a.git("log", "-1", "--format=%s", "--", welPage)
	if err != nil {
		return err
	}
	head, err := a.git("log", "-1", "--format=%s")
	if err != nil {
		return err
	}
	if strings.TrimSpace(subj) != subject || strings.TrimSpace(head) != subject || !a.committed() {
		a.unrecorded = true
		return nil
	}
	files, err := a.git("show", "--name-only", "--format=", "HEAD")
	if err != nil {
		return err
	}
	if strings.TrimSpace(files) != welPage {
		a.sweptrun = true
	}
	return nil
}

// --- Review ---------------------------------------------------------

func (a *wikiEditAdapter) ReadReview() error {
	if !a.gate.pass(a.shown == nil && a.present()) {
		return nil
	}
	f, err := a.flag()
	if err != nil {
		return err
	}
	if !a.gate.pass(f != nil) {
		return nil
	}
	a.shown = f
	return nil
}

// ClaimAction marks the claim Review shows as an inference, with the
// lines it read.
func (a *wikiEditAdapter) ClaimAction() error {
	enabled := a.shown != nil && a.phaseView() != "committing"
	if enabled {
		body, ok, err := a.page()
		if err != nil {
			return err
		}
		enabled = ok && linesAt(body, a.shown)
	}
	if !a.gate.pass(enabled) {
		return nil
	}
	f := a.shown
	err := a.api(http.MethodPost, "/api/wiki/claim", map[string]any{"path": f.Page, "line": f.Line, "end": f.End, "raw": f.Raw, "action": "inference"}, nil)
	if err != nil {
		return err
	}
	a.shown, a.hand = nil, true
	return a.checkHandCommit(fmt.Sprintf("review: mark %s:%d as inference", welPage, f.Line))
}

func (a *wikiEditAdapter) LeaveReview() error {
	if !a.gate.pass(a.shown != nil) {
		return nil
	}
	a.shown = nil
	return nil
}

// --- starting a run -------------------------------------------------

// The three starts are the clicks (and launchd's tick); the process
// itself starts in Lock, which the spec lets happen at any later point.
func (a *wikiEditAdapter) start(name string) error {
	if !a.gate.pass(a.spawned == "") {
		return nil
	}
	a.spawned = name
	return nil
}

func (a *wikiEditAdapter) IngestNow() error     { return a.start("IngestNow") }
func (a *wikiEditAdapter) RefreshBrief() error  { return a.start("RefreshBrief") }
func (a *wikiEditAdapter) SchedulerTick() error { return a.start("SchedulerTick") }

// Lock starts the spawned process and waits for it to get past the
// lock: its headless session takes the turn queued for it, or it exits
// because the lock is held.
func (a *wikiEditAdapter) Lock() error {
	if !a.gate.pass(a.spawned != "") {
		return nil
	}
	how := a.spawned
	a.spawned = ""
	kind := "ingest"
	if how == "RefreshBrief" {
		kind = "brief"
	}
	if kind == "ingest" && a.logged == len(a.pends) {
		// Always something to ingest: a session an hour quiet.
		a.nsess++
		id := fmt.Sprintf("wel-%04d", a.nsess)
		if err := writeSession(filepath.Join(a.s.Home, ".bough", "history", id+".jsonl"), time.Now().Add(-time.Hour), "session "+id); err != nil {
			return err
		}
		a.pends = append(a.pends, id)
	}
	a.turn++
	name := fmt.Sprintf("wel%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: kind + " done"})
	var alive func() bool
	switch how {
	case "SchedulerTick":
		var err error
		if alive, err = a.tick(); err != nil {
			return err
		}
	default:
		match, path := " wiki run", "/api/wiki/ingest"
		if kind == "brief" {
			match, path = " wiki brief", "/api/me/refresh"
		}
		before, err := a.children(match)
		if err != nil {
			return err
		}
		if err := a.api(http.MethodPost, path, nil, nil); err != nil {
			return err
		}
		// spawnWiki's Start returns after exec, so the child is there now.
		after, err := a.children(match)
		if err != nil {
			return err
		}
		pid := 0
		for _, p := range after {
			if !slices.Contains(before, p) {
				pid = p
			}
		}
		alive = func() bool { return pid != 0 && procAlive(pid, match) }
	}
	taken := filepath.Join(a.dir, name+".taken")
	return poll("the spawned "+kind+" to reach the lock", func() (bool, error) {
		if exists(taken) {
			a.run = &welRun{kind: kind, turn: name, phase: "running", alive: alive}
			return true, nil
		}
		if alive() {
			return false, nil
		}
		// It exited without asking the model: take the turn back,
		// unless it was taken between the two checks.
		if err := os.Remove(filepath.Join(a.dir, name+".json")); errors.Is(err, os.ErrNotExist) {
			return false, nil
		}
		return true, nil
	})
}

// tick is launchd's `bough wiki run`: the same binary and HOME as the
// serve, started outside it.
func (a *wikiEditAdapter) tick() (func() bool, error) {
	logf, err := os.OpenFile(filepath.Join(a.wiki, "ingest.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, err
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
	cmd.Env = append(append(cmd.Env, "HOME="+a.s.Home, "BOUGH_WEB_ADDR=127.0.0.1:0"), welGitEnv()...)
	if err := cmd.Start(); err != nil {
		logf.Close()
		return nil, err
	}
	done := make(chan struct{})
	go func() { cmd.Wait(); logf.Close(); close(done) }()
	return func() bool {
		select {
		case <-done:
			return false
		default:
			return true
		}
	}, nil
}

// children lists serve's child processes whose command has match.
func (a *wikiEditAdapter) children(match string) ([]int, error) {
	out, err := exec.Command("ps", "-A", "-o", "pid=,ppid=,command=").Output()
	if err != nil {
		return nil, err
	}
	var pids []int
	for _, line := range strings.Split(string(out), "\n") {
		f := strings.Fields(line)
		if len(f) < 4 || f[1] != strconv.Itoa(a.pid) || !strings.Contains(line, match) {
			continue
		}
		if p, err := strconv.Atoi(f[0]); err == nil {
			pids = append(pids, p)
		}
	}
	return pids, nil
}

// procAlive: a reaped or zombie process no longer counts.
func procAlive(pid int, match string) bool {
	out, err := exec.Command("ps", "-o", "stat=,command=", "-p", strconv.Itoa(pid)).Output()
	return err == nil && !strings.HasPrefix(strings.TrimSpace(string(out)), "Z") && strings.Contains(string(out), match)
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

// --- the run's agent ------------------------------------------------

// RunWritesPage: the agent rewrites P, flagged claim and all, and
// touches the other page.
func (a *wikiEditAdapter) RunWritesPage() error {
	if !a.gate.pass(a.phaseView() == "running" && a.present()) {
		return nil
	}
	a.version++
	if err := os.WriteFile(a.path(welPage), []byte(welPageBody(a.version)), 0o644); err != nil {
		return err
	}
	a.hand = false
	return a.touchOther()
}

// RunDeletesPage: the agent merges P away.
func (a *wikiEditAdapter) RunDeletesPage() error {
	if !a.gate.pass(a.phaseView() == "running" && a.present()) {
		return nil
	}
	if err := os.Remove(a.path(welPage)); err != nil {
		return err
	}
	a.hand = false
	return a.touchOther()
}

func (a *wikiEditAdapter) touchOther() error {
	f, err := os.OpenFile(a.path(welOther), os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = fmt.Fprintf(f, "Run pass %d.\n", a.turn)
	return err
}

// RunEnds: the agent's last writes (an ingest's log headings, a brief's
// page), its turn ends, and Run stages everything and starts the commit,
// which the hook parks.
func (a *wikiEditAdapter) RunEnds() error {
	if !a.gate.pass(a.phaseView() == "running") {
		return nil
	}
	r := a.run
	if r.kind == "ingest" {
		f, err := os.OpenFile(a.path("log.md"), os.O_APPEND|os.O_WRONLY, 0o644)
		if err != nil {
			return err
		}
		for _, id := range a.pends[a.logged:] {
			fmt.Fprintf(f, "\n## [%s] ingest | %s#3 | ingested | -\n", time.Now().Format("2006-01-02"), id)
		}
		f.Close()
		a.logged = len(a.pends)
	} else {
		brief := fmt.Sprintf("# %s\n\nBrief %s.\n", time.Now().Format("2006-01-02"), r.turn)
		if err := os.WriteFile(a.path(wiki.BriefPath(time.Now())), []byte(brief), 0o644); err != nil {
			return err
		}
	}
	control.Release(a.t, a.dir, r.turn)
	r.phase = "committing"
	return poll("the run's commit to start", func() (bool, error) {
		if exists(filepath.Join(a.mark, "committing")) {
			return true, nil
		}
		if !r.alive() {
			return false, fmt.Errorf("the %s run exited without committing; ingest.log:\n%s", r.kind, a.log())
		}
		return false, nil
	})
}

// RunCommits lets the parked commit land and waits for the run to exit
// and release the lock; the commit must not hold a hand edit.
func (a *wikiEditAdapter) RunCommits() error {
	if !a.gate.pass(a.phaseView() == "committing") {
		return nil
	}
	r := a.run
	if err := os.WriteFile(filepath.Join(a.mark, "release"), nil, 0o644); err != nil {
		return err
	}
	if err := poll("the run to commit and exit", func() (bool, error) {
		return !r.alive() && !wikiLockHeld(a.wiki), nil
	}); err != nil {
		return err
	}
	a.run = nil
	subj, err := a.git("log", "-1", "--format=%s")
	if err != nil {
		return err
	}
	if !strings.HasPrefix(subj, r.kind+" ") {
		return fmt.Errorf("the %s run's commit is not HEAD: %q", r.kind, subj)
	}
	diff, err := a.git("diff", "HEAD~1", "HEAD", "--", welPage)
	if err != nil {
		return err
	}
	for _, l := range strings.Split(diff, "\n") {
		if strings.HasPrefix(l, "+") && (strings.Contains(l, "Hand edit") || strings.Contains(l, "*Inference:*")) {
			a.swepthand = true
		}
	}
	return nil
}

func (a *wikiEditAdapter) log() string {
	b, _ := os.ReadFile(filepath.Join(a.wiki, "ingest.log"))
	return string(b)
}

// api is one request to the serve's HTTP API; a non-2xx answer is a
// *servetest.APIError.
func (a *wikiEditAdapter) api(method, path string, body, out any) error {
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

// welAct wraps an action so the walk counts what it really took.
func welAct(name string, f func(*wikiEditAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*wikiEditAdapter)
		err := f(a)
		if !a.gate.off {
			a.ran[name]++
		}
		return nil, err
	}
}

var wikiEditActions = map[string]map[string]fmbt.ActionFunc{"Wiki": {
	"Edit":           welAct("Edit", (*wikiEditAdapter).Edit),
	"Cancel":         welAct("Cancel", (*wikiEditAdapter).Cancel),
	"Reload":         welAct("Reload", (*wikiEditAdapter).Reload),
	"Save":           welAct("Save", (*wikiEditAdapter).Save),
	"ReadReview":     welAct("ReadReview", (*wikiEditAdapter).ReadReview),
	"ClaimAction":    welAct("ClaimAction", (*wikiEditAdapter).ClaimAction),
	"LeaveReview":    welAct("LeaveReview", (*wikiEditAdapter).LeaveReview),
	"IngestNow":      welAct("IngestNow", (*wikiEditAdapter).IngestNow),
	"RefreshBrief":   welAct("RefreshBrief", (*wikiEditAdapter).RefreshBrief),
	"SchedulerTick":  welAct("SchedulerTick", (*wikiEditAdapter).SchedulerTick),
	"Lock":           welAct("Lock", (*wikiEditAdapter).Lock),
	"RunWritesPage":  welAct("RunWritesPage", (*wikiEditAdapter).RunWritesPage),
	"RunDeletesPage": welAct("RunDeletesPage", (*wikiEditAdapter).RunDeletesPage),
	"RunEnds":        welAct("RunEnds", (*wikiEditAdapter).RunEnds),
	"RunCommits":     welAct("RunCommits", (*wikiEditAdapter).RunCommits),
}}

func wikiEditOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// wikiEditHistory reads a run's headless transcript as the steps that
// led to it: its input is the start (Refresh for a brief, Ingest now for
// an ingest: the scheduler's tick is the same process) and the run
// reaching the lock; its done is the agent ending, then the commit that
// releases the lock. The page editor and Review leave no transcript.
func wikiEditHistory(entries []history.Entry) []tracecheck.Step {
	w := func(s string) string { return "Wiki#0." + s }
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Wiki#0.held": 0, "Wiki#0.run": ""}}}
	for _, e := range entries {
		switch e.Kind {
		case "input":
			start := "IngestNow"
			if text, _ := e.Data["text"].(string); strings.HasPrefix(text, "/llm-wiki brief") {
				start = "RefreshBrief"
			}
			steps = append(steps,
				tracecheck.Step{Action: w(start), State: map[string]any{"Wiki#0.spawned": true}},
				tracecheck.Step{Action: w("Lock"), State: map[string]any{"Wiki#0.spawned": false, "Wiki#0.held": 1, "Wiki#0.run": "running"}})
		case "done":
			steps = append(steps,
				tracecheck.Step{Action: w("RunEnds"), State: map[string]any{"Wiki#0.run": "committing", "Wiki#0.dirty": false}},
				tracecheck.Step{Action: w("RunCommits"), State: map[string]any{"Wiki#0.held": 0, "Wiki#0.run": ""}})
		}
	}
	return steps
}

func init() { historyProjections["wiki_edit_vs_ingest_lock"] = wikiEditHistory }

// runSessions is every headless run the walks started: the sessions
// whose cwd is the wiki.
func (a *wikiEditAdapter) runSessions(t *testing.T) []string {
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

// walkWikiEdit walks every path over the checked-in graph for cover and
// returns a line per path that failed (only the first with stopAtFirst).
func (a *wikiEditAdapter) walkAll(t *testing.T, cover tracecheck.Cover, stopAtFirst bool) []string {
	t.Helper()
	b, err := pathsJSONCover("wiki_edit_vs_ingest_lock", cover)
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
				names = append(names, strings.TrimPrefix(s.Action, "Wiki#0."))
			}
			fails = append(fails, fmt.Sprintf("path %d [%s]: %v", i, strings.Join(names, " "), err))
			if stopAtFirst {
				break
			}
		}
	}
	t.Logf("wiki_edit_vs_ingest_lock: %d paths, %d failed; actions taken: %v", len(doc.Paths), len(fails), a.ran)
	return fails
}

// walk replays one path: every action must pass its gate, and after
// every step the state read must be the path's.
func (a *wikiEditAdapter) walk(trace []tracecheck.Step) (err error) {
	defer func() {
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("Cleanup: %w", cerr)
		}
	}()
	for i, step := range trace {
		name := strings.TrimPrefix(step.Action, "Wiki#0.")
		if i == 0 {
			err = a.Init()
		} else if f := wikiEditActions["Wiki"][name]; f == nil {
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
			field, ok := strings.CutPrefix(k, "Wiki#0.")
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

// TestWikiEditVsIngestLock is the fizzbee-mbt random run; it is part
// of the exhaustive MODEL_COVER=transitions run (see runMBT).
func TestWikiEditVsIngestLock(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiEditAdapter(t)
	if err := runMBT(t, "wiki_edit_vs_ingest_lock", a, wikiEditActions, wikiEditOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// TestWikiEditVsIngestLockPaths walks the generated paths (every state,
// or every transition under MODEL_COVER=transitions) against a real
// serve, then trace-checks the transcript of every run they started.
func TestWikiEditVsIngestLockPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiEditAdapter(t)
	for _, f := range a.walkAll(t, envCover(), false) {
		t.Error(f)
	}
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "wiki_edit_vs_ingest_lock"))
	if err != nil {
		t.Fatal(err)
	}
	ids := a.runSessions(t)
	if len(ids) == 0 {
		t.Fatal("the walks started no run")
	}
	for _, id := range ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), wikiEditHistory)
	}
	t.Logf("wiki_edit_vs_ingest_lock: trace-checked %d runs", len(ids))
}

// The walk proves nothing unless an adapter that breaks the model fails
// it: here Save sends no base, as the editor did, so a Save over a
// run's rewrite lands instead of answering 409. The stale editor is a
// state every states-cover reaches only through that Save.
func TestWikiEditVsIngestLockCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiEditAdapter(t)
	a.bug = "save-no-base"
	fails := a.walkAll(t, tracecheck.CoverStates, true)
	if len(fails) == 0 {
		t.Fatal("every path passed with a Save that sends no base; the walk is not checking state")
	}
	t.Log(fails[0])
}

// The projection is only a check if a transcript the model forbids is
// refused: a run that ended without ever reaching the lock.
func TestWikiEditVsIngestLockHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("x")), "..", "testdata", "wiki_edit_vs_ingest_lock"))
	if err != nil {
		t.Fatal(err)
	}
	input := history.Entry{Kind: "input", Data: map[string]any{"text": "/llm-wiki ingest s1"}}
	brief := history.Entry{Kind: "input", Data: map[string]any{"text": "/llm-wiki brief"}}
	done := history.Entry{Kind: "done"}
	for name, es := range map[string][]history.Entry{
		"ingest": {input, done},
		"brief":  {brief, done},
		"held":   {input},
	} {
		if v := g.Check(wikiEditHistory(es)); v != nil {
			t.Errorf("%s: %v", name, v)
		}
	}
	if v := g.Check(wikiEditHistory([]history.Entry{done})); v == nil {
		t.Error("a run that committed without taking the lock passed the trace check")
	} else {
		t.Logf("refused as expected: %v", v)
	}
}
