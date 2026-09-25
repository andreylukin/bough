package orb

import "github.com/andreylukin/bough/internal/container"

// StubHost is for a driver outside this package that mounts the orb row
// in its own process to play a project session (tests/model/mbt), as
// iorb.StubHost is for the host CLIs: home stands for the user's home,
// chdir for the move into the primary worktree (process-global, where
// the driver runs a session per walk) and runtime for the backend
// `runtime: fake` names. Call it before any orb row in the process
// applies.
func StubHost(home func() (string, error), cd func(string) error, runtime func() container.Runtime) {
	userHome, chdir, newFake = home, cd, runtime
}

// Stashed reports whether session's handle sits in opened: kept by a
// same-config reload for the next Apply, owned by no row until then.
func Stashed(session string) bool {
	opened.Lock()
	defer opened.Unlock()
	_, ok := opened.m[session]
	return ok
}
