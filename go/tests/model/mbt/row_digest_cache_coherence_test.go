//go:build !windows

package mbt

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/row_digest_cache_coherence.fizz: /api/sessions merges a cached
// digest (keyed on the transcript file's size/mtime) with the child's
// liveness, read fresh on every request (internal/serve/digest.go,
// api.go rowOf). The finding to check for is a served row that shows an
// ask as still open once the transcript has resolved it.
//
// askOpen (the spec's transcript truth) and digestAskOpen (what the
// digest last computed) are both read straight off the session's
// history file, ignoring the child's liveness: internal/serve/status.go
// StatusOf deliberately hides a pending ask once the child that asked
// it is gone (a dead child cannot read the answer), which is a real,
// separate, well-commented feature, not the digest race this flow is
// about — reading the row through the API here would fold that masking
// into askOpen and make Die look like it closes the ask. live is read
// off the server, which is the one thing the flow needs from it: proof
// that liveness flipping never corrupts the digest/transcript
// agreement the assertions check.

// rdccAdapter plays one session: an ask is armed as soon as it starts,
// Resolve expires it (as the real timeout would), Recompute reads the
// row fresh, and Die/Revive crash and restart the child independently
// of the ask.
type rdccAdapter struct {
	t      *testing.T
	s      *servetest.Server
	dir    string // llm-control's queue
	expire string // BOUGH_TEST_ASK_EXPIRE_DIR
	gate   gate

	id     string
	cwd    string // this walk's own, so its child can be found to kill
	turn   int
	held   string // the model request held in flight, "" when none
	next   string // the block turn queued for the next request
	openID string // the armed ask's id, "" once resolved
	ids    []string

	cacheKey, digestKey int
	digestAskOpen       bool

	// noRecompute is the deliberate bug
	// TestRowDigestCacheCoherenceCatchesWrongAdapter injects: Recompute
	// takes the cache key but never rereads the ask.
	noRecompute bool
}

func newRDCCAdapter(t *testing.T) *rdccAdapter {
	expire := t.TempDir()
	s := servetest.Start(t, servetest.Options{
		Config: controlConfig,
		Env:    []string{"BOUGH_TEST_ASK_EXPIRE_DIR=" + expire},
	})
	return &rdccAdapter{t: t, s: s, dir: control.Dir(s.Home), expire: expire}
}

// queueNext keeps one block turn queued, so whatever request the engine
// makes next (after the ask times out, or a respawn) is held.
func (a *rdccAdapter) queueNext() {
	a.turn++
	a.next = fmt.Sprintf("t%05d", a.turn)
	control.Queue(a.t, a.dir, a.next, control.Turn{Mode: "block", Text: "finished " + a.next})
}

func (a *rdccAdapter) takeNext() error {
	if err := waitTaken(a.dir, a.next); err != nil {
		return err
	}
	a.held = a.next
	a.queueNext()
	return nil
}

// Init starts each walk on a fresh session whose first turn immediately
// asks: Session.Init has askOpen=True, live=True, both keys at 0 and
// digestAskOpen agreeing with askOpen, matching a row nobody has ever
// read yet.
func (a *rdccAdapter) Init() error {
	a.queueNext()
	ctx, cancel := actionCtx()
	defer cancel()
	a.cwd = a.s.Dir(a.t, fmt.Sprintf("w%d", len(a.ids)))
	row, err := a.s.CreateSession(ctx, a.cwd, "start "+a.next)
	if err != nil {
		return err
	}
	a.id, a.held = row.ID, ""
	if err := a.takeNext(); err != nil {
		return err
	}
	control.ReleaseWith(a.t, a.dir, a.held, control.Turn{Mode: "call", Tool: "ask", Args: map[string]any{
		"question": "Which colour?", "options": []string{"red", "blue"},
	}})
	a.held = ""
	row, err = waitRow(a.s, a.id, "needs-you", func(r serve.Row) bool { return r.Status == serve.StatusNeedsYou })
	if err != nil {
		return err
	}
	if row.Ask == nil {
		return fmt.Errorf("init: needs-you row has no ask")
	}
	a.openID = row.Ask.ID
	a.cacheKey, a.digestKey, a.digestAskOpen = 0, 0, true
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

// Cleanup kills the walk's child (whatever it is doing) and takes back
// any turn it left queued, so the next walk's requests are its own.
func (a *rdccAdapter) Cleanup() error {
	a.kill()
	a.held = ""
	if a.next != "" {
		os.Remove(a.dir + "/" + a.next + ".json")
		a.next = ""
	}
	ents, _ := os.ReadDir(a.expire)
	for _, e := range ents {
		os.Remove(a.expire + "/" + e.Name())
	}
	return nil
}

// kill SIGKILLs the session's child, if one is running: a crash, found
// by its cwd since a created session's command line does not carry its
// id.
func (a *rdccAdapter) kill() error {
	out, _ := exec.Command("lsof", "-a", "-d", "cwd", "-c", "bough", "-Fpn").Output()
	pid := 0
	for _, l := range strings.Split(string(out), "\n") {
		switch {
		case strings.HasPrefix(l, "p"):
			pid, _ = strconv.Atoi(l[1:])
		case strings.HasPrefix(l, "n") && l[1:] == a.cwd && pid > 0:
			syscall.Kill(pid, syscall.SIGKILL)
		}
	}
	_, err := waitRow(a.s, a.id, "the child to be gone", func(r serve.Row) bool { return !r.Live })
	return err
}

func (a *rdccAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// rawAskOpen is the transcript's own answer, straight off history: the
// ask (a "call" entry for tool "ask" — its live "phase":"start" is
// never recorded, only its close) is still open until that close lands,
// whatever the child is doing right now.
func (a *rdccAdapter) rawAskOpen() (bool, error) {
	entries, err := history.Read(filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl"))
	if err != nil {
		return false, err
	}
	for _, e := range entries {
		if e.Kind == "call" && str(e.Data["tool"]) == "ask" {
			return false, nil
		}
	}
	return true, nil
}

// GetState is the Session role's state: askOpen off the real transcript
// on every read (never the API's live-merged row — see the file
// comment), live off the real row. cacheKey, digestKey and
// digestAskOpen are the digest cache's own bookkeeping, which nothing
// in the API exposes directly, so the adapter tracks them the way it
// tracks "viewing" in the worked example: cacheKey advances with
// Resolve, the transcript event that always changes the file's (size,
// mtime); digestKey and digestAskOpen are set by Recompute, from a real
// read taken right then.
func (a *rdccAdapter) GetState() (map[string]any, error) {
	open, err := a.rawAskOpen()
	if err != nil {
		return nil, err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"askOpen":       open,
		"live":          row.Live,
		"cacheKey":      a.cacheKey,
		"digestKey":     a.digestKey,
		"digestAskOpen": a.digestAskOpen,
	}, nil
}

// Each action asks the gate with the spec's require first: the runner
// picks among all five at random, disabled ones included.

// Resolve times the open ask out, as its real timeout would: only a
// live child polls BOUGH_TEST_ASK_EXPIRE_DIR, so this needs the child
// alive to see it and write the closing entry. It keeps the model's
// next request held so the child is not left hanging.
func (a *rdccAdapter) Resolve() error {
	open, err := a.rawAskOpen()
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if !a.gate.pass(open && row.Live) {
		return nil
	}
	if err := os.WriteFile(a.expire+"/"+a.openID, nil, 0o644); err != nil {
		return err
	}
	a.cacheKey++
	if err := a.waitRawResolved(); err != nil {
		return err
	}
	return a.takeNext()
}

// waitRawResolved polls history until the ask's closing entry lands.
func (a *rdccAdapter) waitRawResolved() error {
	deadline := time.Now().Add(actionTimeout)
	for time.Now().Before(deadline) {
		if open, err := a.rawAskOpen(); err != nil {
			return err
		} else if !open {
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("waiting for the ask to close: not resolved after %s", actionTimeout)
}

// Recompute is a real read right now: internal/serve/digest.go
// recomputes synchronously whenever the transcript's identity has
// moved past what it last cached, so the read this takes is what the
// digest is caught up to as of this instant.
func (a *rdccAdapter) Recompute() error {
	if !a.gate.pass(a.digestKey != a.cacheKey) {
		return nil
	}
	a.digestKey = a.cacheKey
	if a.noRecompute {
		return nil // the bug: digestAskOpen left stuck at whatever it was
	}
	open, err := a.rawAskOpen()
	if err != nil {
		return err
	}
	a.digestAskOpen = open
	return nil
}

func (a *rdccAdapter) Die() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if !a.gate.pass(row.Live) {
		return nil
	}
	a.held = ""
	return a.kill()
}

// Revive respawns the child with a new prompt: the crashed child's
// pending arm is dropped when the supervisor reaps it
// (internal/serve/supervisor.go drop), so Send never refuses this on
// ErrPendingAsk, whether or not the transcript's ask is still open.
func (a *rdccAdapter) Revive() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return err
	}
	if !a.gate.pass(!row.Live) {
		return nil
	}
	a.queueNext()
	if err := a.s.Prompt(ctx, a.id, "revive "+a.next); err != nil {
		return err
	}
	if err := a.takeNext(); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "live again", func(r serve.Row) bool { return r.Live })
	return err
}

var rdccActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Resolve":   action((*rdccAdapter).Resolve),
	"Recompute": action((*rdccAdapter).Recompute),
	"Die":       action((*rdccAdapter).Die),
	"Revive":    action((*rdccAdapter).Revive),
}}

// The walk is real turns through a real serve (a kill and a respawn
// among them), so the default run stays short.
func rdccOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// rdccHistory reads the abstract trace off a transcript: the "ask"
// entry Init's own call opened is already folded into Init, and the
// call's own end (a "call" entry for tool "ask" — its live "phase":
// "start" is never recorded, only its close) is Resolve, whether it
// closed by timeout or answer. Recompute, Die and Revive never write an
// entry: they only change what the live supervisor and the digest
// cache separately report, so the check is on askOpen alone, which is
// all history can attest to.
func rdccHistory(entries []history.Entry) []tracecheck.Step {
	st := func(open bool) map[string]any { return map[string]any{"Session#0.askOpen": open} }
	steps := []tracecheck.Step{{Action: "Init", State: st(true)}}
	for _, e := range entries {
		if e.Kind == "call" && str(e.Data["tool"]) == "ask" {
			steps = append(steps, tracecheck.Step{Action: "Session#0.Resolve", State: st(false)})
		}
	}
	return steps
}

func init() { historyProjections["row_digest_cache_coherence"] = rdccHistory }

func TestRowDigestCacheCoherence(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRDCCAdapter(t)
	if err := runMBT(t, "row_digest_cache_coherence", a, rdccActions, rdccOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "row_digest_cache_coherence"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), rdccHistory)
	}
}

// The run above proves nothing unless a server whose digest lies about
// an open ask fails it: this adapter reports digestAskOpen as fixed
// True forever, the kind of wiring bug where Recompute never actually
// reads the row.
func TestRowDigestCacheCoherenceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRDCCAdapter(t)
	a.noRecompute = true
	if err := runMBT(t, "row_digest_cache_coherence", a, rdccActions, rdccOptions()); err == nil {
		t.Fatal("a run whose Recompute never updates digestAskOpen passed; the runner is not checking state")
	}
}
