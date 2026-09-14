package container

import (
	"context"
	"fmt"
	"io"
	"os/exec"
)

// stubErr is the one error every not-yet-implemented engine returns, so
// callers see a clear message instead of a half-working runtime.
func stubErr(name string) error { return fmt.Errorf("container: %s: %w", name, ErrNotImplemented) }

// stubCommand returns a cmd whose Err is set, so Start/Run fail before
// anything executes on the host.
func stubCommand(ctx context.Context, name string) *exec.Cmd {
	cmd := exec.CommandContext(ctx, "false")
	cmd.Err = stubErr(name)
	return cmd
}

// Nerdctl is the Linux nerdctl backend (interface-ready, not implemented).
type Nerdctl struct{}

// Podman is the Linux podman backend (interface-ready, not implemented).
type Podman struct{}

// Unsupported is what Default returns on an OS with no known engine.
type Unsupported struct{ OS string }

func (Unsupported) Name() string    { return "unsupported" }
func (u Unsupported) label() string { return "unsupported os " + u.OS }

func (Nerdctl) Name() string                                          { return "nerdctl" }
func (Nerdctl) label_() string                                        { return "nerdctl" }
func (u Nerdctl) Available(context.Context) error                     { return stubErr(u.label_()) }
func (u Nerdctl) Build(context.Context, BuildSpec, io.Writer) error   { return stubErr(u.label_()) }
func (u Nerdctl) Commit(context.Context, CommitSpec, io.Writer) error { return stubErr(u.label_()) }
func (u Nerdctl) ImageExists(context.Context, string) (bool, error) {
	return false, stubErr(u.label_())
}
func (u Nerdctl) Start(context.Context, RunSpec) error { return stubErr(u.label_()) }
func (u Nerdctl) Stop(context.Context, string) error   { return stubErr(u.label_()) }
func (u Nerdctl) Remove(context.Context, string) error { return stubErr(u.label_()) }
func (u Nerdctl) Inspect(context.Context, string) (State, error) {
	return StateMissing, stubErr(u.label_())
}
func (u Nerdctl) CreateVolume(context.Context, string) error { return stubErr(u.label_()) }
func (u Nerdctl) Command(ctx context.Context, _ string, _ ExecOptions, _ ...string) *exec.Cmd {
	return stubCommand(ctx, u.label_())
}

func (Podman) Name() string                                          { return "podman" }
func (Podman) label_() string                                        { return "podman" }
func (u Podman) Available(context.Context) error                     { return stubErr(u.label_()) }
func (u Podman) Build(context.Context, BuildSpec, io.Writer) error   { return stubErr(u.label_()) }
func (u Podman) Commit(context.Context, CommitSpec, io.Writer) error { return stubErr(u.label_()) }
func (u Podman) ImageExists(context.Context, string) (bool, error)   { return false, stubErr(u.label_()) }
func (u Podman) Start(context.Context, RunSpec) error                { return stubErr(u.label_()) }
func (u Podman) Stop(context.Context, string) error                  { return stubErr(u.label_()) }
func (u Podman) Remove(context.Context, string) error                { return stubErr(u.label_()) }
func (u Podman) Inspect(context.Context, string) (State, error) {
	return StateMissing, stubErr(u.label_())
}
func (u Podman) CreateVolume(context.Context, string) error { return stubErr(u.label_()) }
func (u Podman) Command(ctx context.Context, _ string, _ ExecOptions, _ ...string) *exec.Cmd {
	return stubCommand(ctx, u.label_())
}
func (u Unsupported) label_() string                                      { return u.label() }
func (u Unsupported) Available(context.Context) error                     { return stubErr(u.label_()) }
func (u Unsupported) Build(context.Context, BuildSpec, io.Writer) error   { return stubErr(u.label_()) }
func (u Unsupported) Commit(context.Context, CommitSpec, io.Writer) error { return stubErr(u.label_()) }
func (u Unsupported) ImageExists(context.Context, string) (bool, error) {
	return false, stubErr(u.label_())
}
func (u Unsupported) Start(context.Context, RunSpec) error { return stubErr(u.label_()) }
func (u Unsupported) Stop(context.Context, string) error   { return stubErr(u.label_()) }
func (u Unsupported) Remove(context.Context, string) error { return stubErr(u.label_()) }
func (u Unsupported) Inspect(context.Context, string) (State, error) {
	return StateMissing, stubErr(u.label_())
}
func (u Unsupported) CreateVolume(context.Context, string) error { return stubErr(u.label_()) }
func (u Unsupported) Command(ctx context.Context, _ string, _ ExecOptions, _ ...string) *exec.Cmd {
	return stubCommand(ctx, u.label_())
}
