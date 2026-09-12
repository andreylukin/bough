package serve

// The watcher engine's wiring into serve: where its shell runs, which
// session it wakes, and when it is allowed to run at all. The engine
// itself (internal/serve/watch) is pure; everything that touches a
// child process or the outside world is here.

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/plugins/codemode"
)

// watchWake opens the turn a watcher starts on its own. Without it the
// transcript shows a reply to a question nobody asked; the prefix
// mirrors plugins/loop's jobWake for background jobs.
const watchWake = "[watcher] Something you were watching changed while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\n"

// watchTick is how often the engine looks for due watchers. It is not
// a watcher's interval — each file sets its own, and this only bounds
// how late a due one runs.
const watchTick = time.Second

// watchEvalTimeout bounds one watcher evaluation, and watchRunTimeout
// the shell command it polls with: a hung command must not wedge the
// engine's single goroutine.
const (
	watchEvalTimeout = 5 * time.Second
	watchRunTimeout  = 60 * time.Second
)

// StartWatchers runs the watcher engine until ctx is done. It returns
// the error that stopped it starting (a non-loopback bind, say) so the
// caller can log it — watchers are arbitrary shell and serve has no
// auth, so refusing must be visible, not silent.
func (a *API) StartWatchers(ctx context.Context, addr string) error {
	if err := watch.CheckLoopback(addr); err != nil {
		return err
	}
	e := &watch.Engine{
		Dir:  filepath.Join(a.home, ".bough", "watchers"),
		Eval: codemode.New(watchEvalTimeout),
		Exec: shellExec{},
		Wake: supWaker{a.sup},
		Busy: supWaker{a.sup},
	}
	a.watch = e
	go func() {
		if err := e.Run(ctx, addr, watchTick); err != nil && ctx.Err() == nil {
			fmt.Fprintln(os.Stderr, "bough serve: watchers:", err)
		}
	}()
	return nil
}

// watcherStatus is the "watchers" list of GET /api/hooks; no engine
// (watchers refused, or a test) is an empty list, never a null.
func (a *API) watcherStatus() []watch.WatcherStatus {
	if a.watch == nil {
		return []watch.WatcherStatus{}
	}
	return a.watch.Status()
}

// shellExec runs a watcher's command the way a shell would, so a file
// can say `gh pr checks | head -1` without quoting games.
type shellExec struct{}

func (shellExec) Run(ctx context.Context, cmd string) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, watchRunTimeout)
	defer cancel()
	out, err := exec.CommandContext(ctx, "/bin/sh", "-c", cmd).Output()
	if err != nil {
		return "", err
	}
	return string(out), nil
}

// supWaker delivers a wake into a session's stdin, the same way
// SetModel writes "/model x", and answers whether that session is
// mid-turn. A watcher names no session in v1, so the wake goes to the
// most recently active unarchived one — the session whose work the
// watcher was set up beside.
type supWaker struct{ sup *Supervisor }

func (s supWaker) Wake(session, text string) error {
	id := s.target(session)
	if id == "" {
		return fmt.Errorf("serve: watchers: no session to wake")
	}
	return s.sup.Send(id, watchWake+text)
}

// Idle is the never-interrupt rule, read off the same derived status
// the rest of serve reports. A session waiting on a tools.ask counts
// as busy: a wake would be eaten as the answer.
func (s supWaker) Idle(session string) bool {
	id := s.target(session)
	if id == "" {
		return false
	}
	entries, err := s.sup.Entries(id)
	if err != nil {
		return false
	}
	st, _ := StatusOf(entries, s.sup.Live(id))
	return st != StatusRunning && st != StatusNeedsYou
}

func (s supWaker) target(session string) string {
	if session != "" {
		return session
	}
	infos, err := s.sup.List()
	if err != nil {
		return ""
	}
	best, at := "", time.Time{}
	for _, in := range infos {
		if s.sup.Meta(in.ID).Archived {
			continue
		}
		if best == "" || in.ModTime.After(at) {
			best, at = in.ID, in.ModTime
		}
	}
	return best
}
