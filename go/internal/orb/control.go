package orb

// Shared by serve's orb endpoints and `bough project`, so the CLI and the
// web never disagree on which log a session shows or what Stop does.

import (
	"context"
	"path/filepath"

	"github.com/andreylukin/bough/internal/container"
)

// LogFor is the log that explains a session's orb: resume.log, or the
// project's build.log when the start failed at the image build.
func LogFor(home string, s State) (name, path string) {
	if FailedAt(s) == PhaseBuild && s.Project != "" {
		return "build.log", ImageLogPath(home, s.Project)
	}
	return "resume.log", filepath.Join(Dir(home, s.Session), "resume.log")
}

// StopContainer marks state.json stopped and stops the container, putting
// the state back when the runtime refuses. A container that no longer
// exists is stopped already: state.json said "running" for eight orbs
// whose VMs were long gone, and the reaper retried each every five
// minutes for a week because the failed stop restored "running".
func StopContainer(ctx context.Context, rt container.Runtime, home, session string) error {
	prev, _ := MarkStopped(home, session)
	if err := rt.Stop(ctx, container.OrbName(session)); err != nil {
		if st, ierr := rt.Inspect(ctx, container.OrbName(session)); ierr == nil && st == container.StateMissing {
			return nil
		}
		_ = Restore(home, prev)
		return err
	}
	return nil
}
