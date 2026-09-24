package serve

// Seams for go/tests/model's orb lifecycle model. The model has state
// serve keeps only in memory (the running-container snapshot) and a
// clock the reaper reads, and the model-based test must observe the one
// and step the other; these are the existing methods, exported, and
// nothing in serve calls them.

import (
	"context"
	"time"
)

// ContainerUp is what serve believes about a session's container: the
// running snapshot's answer, taking one when there is none.
func (a *API) ContainerUp(session string) bool { return a.containerUp(session) }

// ForgetRunning drops the running snapshot, as runningTTL passing does.
func (a *API) ForgetRunning() { a.forgetRunning() }

// ReapIdleOrbs is one reaper tick at now.
func (a *API) ReapIdleOrbs(ctx context.Context, idle time.Duration, now time.Time) []string {
	return a.reapIdleOrbs(ctx, idle, now)
}

// SetRunningTTL sets how long a running snapshot answers for. A browser
// walk is slower than runningTTL, and a snapshot that expired mid-walk
// is a Refresh the spec did not take; the walk takes it with
// ForgetRunning instead.
func (a *API) SetRunningTTL(d time.Duration) {
	a.runningMu.Lock()
	defer a.runningMu.Unlock()
	a.runningFor = d
}
