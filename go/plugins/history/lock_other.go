//go:build !unix

package history

import "os"

// lockFile is a no-op where flock is unavailable.
func lockFile(*os.File) (unlock func()) { return func() {} }
