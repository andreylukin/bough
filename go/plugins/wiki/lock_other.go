//go:build !unix

package wiki

// tryLock always succeeds where flock is unavailable: the scheduler is
// launchd-only there anyway.
func tryLock(string) (unlock func(), ok bool) { return func() {}, true }
