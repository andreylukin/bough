package uitest

import (
	"context"
	"os/exec"

	"github.com/andreylukin/bough/kernel"
)

// HostOrb is a test "orb" service that runs commands on the host. Only
// a project session writes files, so a test of writing, the done chip
// or /undo needs project mode; a real container has no place in an
// in-process test.
type HostOrb struct{ Dir string }

// Command runs argv on the host, like the real orb runs it in the
// container: tools sets the process group and kill on the result.
func (o HostOrb) Command(ctx context.Context, argv ...string) *exec.Cmd {
	return exec.CommandContext(ctx, argv[0], argv[1:]...)
}

// Root bounds tools.write and tools.patch, as the orb's worktrees do.
func (o HostOrb) Root() string { return o.Dir }

// ProjectMode makes ctx a project session whose writable root is root:
// call it from Mount's pre func, before tools and history mount.
func ProjectMode(c *kernel.Context, root string) {
	c.Provide("session-mode", "project")
	c.Provide("session-project", "uitest")
	c.Provide("orb", HostOrb{Dir: root})
}
