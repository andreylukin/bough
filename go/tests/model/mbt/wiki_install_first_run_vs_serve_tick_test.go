//go:build !windows

package mbt

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/plugins/wiki"
)

// specs/wiki_install_first_run_vs_serve_tick.fizz against the real
// plugins/wiki.EnsureWikiForTest: a person's `bough wiki install` racing
// launchd's own first `bough wiki run` tick, before either has created
// the wiki directory. Both callers run in real goroutines, paused right
// at ensureWiki's check-then-create boundary (wiki.EnsureWikiForTest's
// atCheckpoint hook) so the adapter can force whatever interleaving the
// spec's ICheck/TCheck/ICreate/TCreate ordering asks for, instead of
// hoping real thread scheduling produces it.
//
// The spec's one role, Wiki#0, carries both callers' fields; there is
// only ever one instance.
type wikiRaceAdapter struct {
	t   *testing.T
	dir string // this walk's wiki directory, fresh per Init
	n   int    // walk counter, for a fresh dir name each Init

	gate gate

	i, tk caller // installer, ticker
}

// caller is one of install()'s or Run()'s calls into ensureWiki: a
// goroutine parked at the checkpoint until the adapter lets it finish.
type caller struct {
	seen string // "unset" | "present" | "missing", the spec's i_seen/t_seen
	done bool

	started  bool
	arrived  chan struct{} // goroutine -> adapter: I'm at the checkpoint
	proceed  chan struct{} // adapter -> goroutine: create-or-skip, then commit
	finished chan error    // goroutine -> adapter: ensureWiki returned
}

func newWikiRaceAdapter(t *testing.T) *wikiRaceAdapter {
	return &wikiRaceAdapter{t: t}
}

// Init starts each walk on a fresh directory: ensureWiki has never run
// here, so both callers' first stat sees "missing" if either checks
// before the other creates.
func (a *wikiRaceAdapter) Init() error {
	if err := a.drain(&a.i); err != nil {
		return err
	}
	if err := a.drain(&a.tk); err != nil {
		return err
	}
	a.n++
	dir := filepath.Join(a.t.TempDir(), fmt.Sprintf("wiki-%d", a.n))
	a.dir = dir
	a.i = caller{seen: "unset"}
	a.tk = caller{seen: "unset"}
	a.gate.reset()
	return nil
}

// Cleanup unparks and drains any caller a walk left mid-flight: the
// runner stops validating (and calling actions) at the first disabled
// one, so a walk can end with a goroutine still parked at the checkpoint.
func (a *wikiRaceAdapter) Cleanup() error {
	if err := a.drain(&a.i); err != nil {
		return err
	}
	return a.drain(&a.tk)
}

func (a *wikiRaceAdapter) drain(c *caller) error {
	if !c.started || c.done {
		return nil
	}
	// c.started && !c.done means check() has already returned, which only
	// happens after its goroutine closed c.arrived and is about to (or
	// already does) block on <-c.proceed: the receive is coming, so this
	// send must not have a non-blocking escape (a select+default here
	// raced the goroutine reaching that receive and could skip the send,
	// leaving <-c.finished below waiting forever).
	c.proceed <- struct{}{}
	err := <-c.finished
	// The framework calls Cleanup after a walk and Init before the next
	// one; both drain, and without this a second drain on the same
	// undrained caller would resend to a goroutine that already exited
	// after its one receive, blocking forever with nothing left to
	// unblock it.
	c.done = true
	return err
}

func (a *wikiRaceAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Wiki", Index: 0}: a}, nil
}

// GetState reads present and the commit tally straight off the real
// directory and its git log: the ground truth two racing callers must
// agree on, however the check-then-create windows interleaved. i_seen,
// t_seen, i_done and t_done are the adapter's own bookkeeping of what
// each caller's stat saw, the same way an example adapter tracks
// "viewing" (README: "tracked by the adapter when it is the client's
// own state").
func (a *wikiRaceAdapter) GetState() (map[string]any, error) {
	present := false
	if _, err := os.Stat(filepath.Join(a.dir, ".git")); err == nil {
		present = true
	}
	commits, err := commitCount(a.dir)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"present":   present,
		"i_seen":    a.i.seen,
		"t_seen":    a.tk.seen,
		"i_done":    a.i.done,
		"t_done":    a.tk.done,
		"committed": commits > 0,
		"commits":   commits,
	}, nil
}

// commitCount is the wiki repo's commit tally, 0 before `git init` has
// run at all.
func commitCount(dir string) (int, error) {
	if _, err := os.Stat(filepath.Join(dir, ".git")); err != nil {
		return 0, nil
	}
	out, err := exec.Command("git", "-C", dir, "log", "--oneline").Output()
	if err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			return 0, nil // an empty repo: `git log` on no commits
		}
		return 0, err
	}
	out = []byte(strings.TrimSpace(string(out)))
	if len(out) == 0 {
		return 0, nil
	}
	return len(strings.Split(string(out), "\n")), nil
}

// check starts c's real EnsureWikiForTest call on first use and blocks
// until it reaches the checkpoint (its stat done, nothing created yet),
// then records what it saw. Called by ICheck/TCheck.
func (a *wikiRaceAdapter) check(c *caller) error {
	if !c.started {
		c.started = true
		c.arrived = make(chan struct{})
		c.proceed = make(chan struct{})
		c.finished = make(chan error, 1)
		dir := a.dir
		go func() {
			c.finished <- wiki.EnsureWikiForTest(dir, func() {
				close(c.arrived)
				<-c.proceed
			})
		}()
	}
	<-c.arrived
	present := false
	if _, err := os.Stat(filepath.Join(a.dir, ".git")); err == nil {
		present = true
	}
	if present {
		c.seen = "present"
	} else {
		c.seen = "missing"
	}
	return nil
}

// finish lets c's parked goroutine create-or-skip and commit, then waits
// for the real call to return. Called by ICreate/ISkip/TCreate/TSkip.
func (a *wikiRaceAdapter) finish(c *caller) error {
	c.proceed <- struct{}{}
	if err := <-c.finished; err != nil {
		return err
	}
	c.done = true
	return nil
}

func (a *wikiRaceAdapter) ICheck() error {
	if !a.gate.pass(a.i.seen == "unset") {
		return nil
	}
	return a.check(&a.i)
}

func (a *wikiRaceAdapter) ICreate() error {
	if !a.gate.pass(a.i.seen == "missing" && !a.i.done) {
		return nil
	}
	return a.finish(&a.i)
}

func (a *wikiRaceAdapter) ISkip() error {
	if !a.gate.pass(a.i.seen == "present" && !a.i.done) {
		return nil
	}
	return a.finish(&a.i)
}

func (a *wikiRaceAdapter) TCheck() error {
	if !a.gate.pass(a.tk.seen == "unset") {
		return nil
	}
	return a.check(&a.tk)
}

func (a *wikiRaceAdapter) TCreate() error {
	if !a.gate.pass(a.tk.seen == "missing" && !a.tk.done) {
		return nil
	}
	return a.finish(&a.tk)
}

func (a *wikiRaceAdapter) TSkip() error {
	if !a.gate.pass(a.tk.seen == "present" && !a.tk.done) {
		return nil
	}
	return a.finish(&a.tk)
}

var wikiRaceActions = map[string]map[string]fmbt.ActionFunc{"Wiki": {
	"ICheck":  action((*wikiRaceAdapter).ICheck),
	"ICreate": action((*wikiRaceAdapter).ICreate),
	"ISkip":   action((*wikiRaceAdapter).ISkip),
	"TCheck":  action((*wikiRaceAdapter).TCheck),
	"TCreate": action((*wikiRaceAdapter).TCreate),
	"TSkip":   action((*wikiRaceAdapter).TSkip),
}, "": {
	// deadlock_detection is off, so fizz links a terminal state (both
	// callers done) to itself as "end", and the runner offers it
	// everywhere. Taken, it matches no link and the server stops
	// checking there, so it declines like a disabled pick (see
	// ask_across_reload_respawn_test.go).
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*wikiRaceAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

// Every action is a real, if parked, call into ensureWiki, so a walk is
// fast; this spec has no model turns to hold it up either.
func wikiRaceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// wikiRaceWrongAdapterOptions needs more walks than wikiRaceOptions: the
// wrong adapter only misbehaves on a walk that reaches i_seen=="present"
// (ICreate or TCreate must land first) and then takes ISkip/TSkip, a
// longer chain than most of this flow's states need, so 200 short random
// walks sometimes never hit it.
func wikiRaceWrongAdapterOptions() map[string]any {
	return map[string]any{"max-seq-runs": 2000, "max-actions": 8, "max-parallel-runs": 0}
}

// This flow has no browser spec and writes no session history, so it
// registers no historyProjections entry; TestHistoryTraces only checks
// specs that do.

func TestWikiInstallFirstRunVsServeTick(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRaceAdapter(t)
	if err := runMBT(t, "wiki_install_first_run_vs_serve_tick", a, wikiRaceActions, wikiRaceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a real disagreement with the model
// fails it. This adapter's ISkip pretends the component was created
// (like ICreate would), the kind of wrong wiring a flow's adapter can
// have, and the run must say so.
func TestWikiInstallFirstRunVsServeTickCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newWikiRaceAdapter(t)
	bad := &wrongSkipAdapter{wikiRaceAdapter: a}
	actions := map[string]map[string]fmbt.ActionFunc{"Wiki": {
		"ICheck":  action((*wrongSkipAdapter).ICheck),
		"ICreate": action((*wrongSkipAdapter).ICreate),
		"ISkip":   action((*wrongSkipAdapter).ISkip),
		"TCheck":  action((*wrongSkipAdapter).TCheck),
		"TCreate": action((*wrongSkipAdapter).TCreate),
		"TSkip":   action((*wrongSkipAdapter).TSkip),
	}, "": {
		"end": func(m any, _ []fmbt.Arg) (any, error) {
			m.(*wrongSkipAdapter).gate.pass(false)
			return nil, errDisabled
		},
	}}
	if err := runMBT(t, "wiki_install_first_run_vs_serve_tick", bad, actions, wikiRaceWrongAdapterOptions()); err == nil {
		t.Fatal("a run whose ISkip reports i_seen=missing passed; the runner is not checking state")
	}
}

// wrongSkipAdapter wraps wikiRaceAdapter and reports the wrong i_seen for
// TestWikiInstallFirstRunVsServeTickCatchesWrongAdapter: whatever the real
// stat saw, GetState always claims "missing", which the spec forbids once
// i_done is set from an ISkip/TSkip (that path requires i_seen=="present").
type wrongSkipAdapter struct {
	*wikiRaceAdapter
}

func (w *wrongSkipAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Wiki", Index: 0}: w}, nil
}

func (w *wrongSkipAdapter) GetState() (map[string]any, error) {
	st, err := w.wikiRaceAdapter.GetState()
	if err != nil {
		return nil, err
	}
	if w.i.done {
		st["i_seen"] = "missing"
	}
	if w.tk.done {
		st["t_seen"] = "missing"
	}
	return st, nil
}
