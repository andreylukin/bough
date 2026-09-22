package serve

// The idle-orb reaper. An orb is a VM, and on a laptop each one holds
// tens of thousands of file descriptors; nothing stopped them when their
// session went quiet, so a week's threads left twenty VMs running and
// the machine out of files. serve now stops the container of any orb
// whose session has been quiet for longer than the idle limit. Nothing
// is lost: the session's next command starts the container again, the
// worktrees stay, and state.json records the stop the way a Stop orb
// click does.

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/andreylukin/bough/internal/orb"
)

const (
	// DefaultOrbIdle is how long a session may be quiet before its orb
	// is stopped, absent BOUGH_ORB_IDLE.
	DefaultOrbIdle = 4 * time.Hour
	reapEvery      = 5 * time.Minute
	reapStopWait   = 60 * time.Second
)

// StartReaper stops idle orbs every few minutes until ctx ends. idle
// <= 0 turns the reaper off.
func (a *API) StartReaper(ctx context.Context, idle time.Duration) {
	if idle <= 0 {
		return
	}
	go func() {
		t := time.NewTicker(reapEvery)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				for _, id := range a.reapIdleOrbs(ctx, idle, time.Now()) {
					fmt.Fprintf(os.Stderr, "bough serve: stopped the orb of %s: idle for %s\n", id, idle)
				}
			}
		}
	}()
}

// reapIdleOrbs stops every running orb whose session has been quiet —
// no history written and no state change — for at least idle. It
// returns the sessions it stopped. A running turn writes history as it
// goes, so a session mid-command is never quiet; a child process that
// is alive but has said nothing for hours is exactly what this is for.
func (a *API) reapIdleOrbs(ctx context.Context, idle time.Duration, now time.Time) []string {
	home := a.sup.Home()
	dirs, err := os.ReadDir(filepath.Join(home, ".bough", "orbs"))
	if err != nil {
		return nil
	}
	var stopped []string
	for _, d := range dirs {
		if !d.IsDir() {
			continue
		}
		st, err := orb.ReadState(home, d.Name())
		if err != nil || st.Session == "" || st.Status != orb.StatusRunning {
			continue
		}
		last := st.UpdatedAt
		if h, err := os.Stat(filepath.Join(a.sup.opt.HistDir, st.Session+".jsonl")); err == nil && h.ModTime().After(last) {
			last = h.ModTime()
		}
		if now.Sub(last) < idle {
			continue
		}
		sctx, cancel := context.WithTimeout(ctx, reapStopWait)
		err = a.sup.stopOrb(sctx, st.Session)
		cancel()
		if err != nil {
			fmt.Fprintf(os.Stderr, "bough serve: reaper: stop orb of %s: %v\n", st.Session, err)
			continue
		}
		stopped = append(stopped, st.Session)
	}
	if len(stopped) > 0 {
		a.forgetRunning()
	}
	return stopped
}

// OrbIdleFromEnv reads BOUGH_ORB_IDLE: a Go duration ("4h", "90m"), "0"
// or "off" to disable, unset for the default.
func OrbIdleFromEnv(v string) (time.Duration, error) {
	switch v {
	case "":
		return DefaultOrbIdle, nil
	case "0", "off", "false":
		return 0, nil
	}
	d, err := time.ParseDuration(v)
	if err != nil {
		return 0, fmt.Errorf("BOUGH_ORB_IDLE=%q: %w", v, err)
	}
	return d, nil
}
