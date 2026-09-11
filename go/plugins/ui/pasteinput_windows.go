package ui

import "time"

// waitReadable never waits on Windows: a split paste-start is flushed
// as before.
func waitReadable(fd uintptr, d time.Duration) bool { return false }
