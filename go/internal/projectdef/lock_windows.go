package projectdef

import (
	"fmt"
	"os"
	"path/filepath"
	"sync"
)

// lockMu stands in for lock_unix.go's flock: Windows has no flock on a
// directory, so writers are serialised within one process only.
var lockMu sync.Mutex

func locked(home, slug string, fn func() error) error {
	if _, err := os.Stat(filepath.Join(Root(home), slug)); err != nil {
		return fmt.Errorf("projectdef: lock %s: %w", slug, err)
	}
	lockMu.Lock()
	defer lockMu.Unlock()
	return fn()
}
