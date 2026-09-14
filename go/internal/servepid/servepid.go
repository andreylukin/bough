// Package servepid reads the "<pid> <addr>\t<cwd>\t<config>\t<caps>"
// record `bough serve` and `bough web` leave in ~/.bough. It lives
// outside cmd/bough so a plugin (workers finding serve) can read the
// same record without importing the command.
package servepid

import (
	"fmt"
	"strings"
)

// Parse reads "<pid> <addr>[\t<cwd>\t<config>\t<caps>]" — tabs, because
// a path may contain spaces. The trailing fields are what a detached
// session is attached to: without them `bough web` in one directory
// silently hands you the session someone started in another.
func Parse(s string) (pid int, addr, dir, config, caps string, err error) {
	parts := strings.Split(strings.TrimRight(s, "\n"), "\t")
	head := strings.Fields(parts[0])
	if len(head) != 2 {
		return 0, "", "", "", "", fmt.Errorf("malformed pidfile: %q", s)
	}
	if _, err := fmt.Sscanf(head[0], "%d", &pid); err != nil || pid <= 0 {
		return 0, "", "", "", "", fmt.Errorf("malformed pidfile pid: %q", head[0])
	}
	if len(parts) > 1 {
		dir = parts[1]
	}
	if len(parts) > 2 {
		config = parts[2]
	}
	if len(parts) > 3 {
		caps = parts[3]
	}
	return pid, head[1], dir, config, caps, nil
}
