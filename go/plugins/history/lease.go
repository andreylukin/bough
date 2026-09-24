package history

import (
	"fmt"
	"path/filepath"
	"strings"
)

// ErrLeased is a session another process has open (see TakeLease).
type ErrLeased struct {
	ID  string
	Pid int
}

func (e *ErrLeased) Error() string {
	return fmt.Sprintf("history: session %s is open in another process (pid %d)", e.ID, e.Pid)
}

// leasePath is where the lease of the session file at path lives: a
// directory of its own, so nothing listing *.jsonl ever sees it.
func leasePath(path string) string {
	return filepath.Join(filepath.Dir(path), ".lease", strings.TrimSuffix(filepath.Base(path), ".jsonl"))
}
