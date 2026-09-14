// Package container is the orb runtime seam: a small interface over a
// container engine CLI, so project sessions can build a snapshot image,
// keep one long-lived container per session and exec commands in it.
// Backends shell out to their CLI; no cgo, no daemon SDKs.
package container

import (
	"context"
	"errors"
	"io"
	"os/exec"
	"runtime"
)

// Runtime is one container engine.
type Runtime interface {
	Name() string // "apple", "nerdctl", "podman", "fake"
	// Available reports nil when the engine is installed AND usable.
	// The error says what to run to fix it.
	Available(ctx context.Context) error
	// Build builds an image from a Dockerfile, tagging it spec.Tag.
	// Output streams to log line by line as it arrives.
	Build(ctx context.Context, spec BuildSpec, log io.Writer) error
	// Commit runs spec.Script on spec.Base and snapshots the result as
	// spec.Tag (setup-script path).
	Commit(ctx context.Context, spec CommitSpec, log io.Writer) error
	ImageExists(ctx context.Context, tag string) (bool, error)
	// Start creates and starts a long-lived container named spec.Name;
	// an existing stopped one of that name is started instead.
	Start(ctx context.Context, spec RunSpec) error
	// Command returns an unstarted *exec.Cmd that runs argv inside the
	// named container. The caller owns Stdout/Stderr/process group/timeout.
	Command(ctx context.Context, name string, opt ExecOptions, argv ...string) *exec.Cmd
	Stop(ctx context.Context, name string) error
	Remove(ctx context.Context, name string) error // missing is nil
	Inspect(ctx context.Context, name string) (State, error)
	CreateVolume(ctx context.Context, name string) error // exists is nil
}

type BuildSpec struct {
	Dir        string // build context
	Dockerfile string // path, usually Dir/Dockerfile
	Tag        string
}

type CommitSpec struct {
	Base   string // generic base image, DefaultBase when empty
	Script string // host path of the setup script
	// Files are extra host files copied into the build context next to
	// the script, as their base names under <parent dir>/<name>.
	Files []string
	// FilesRoot, when set, keeps each of Files at its path relative to
	// it, so a subdirectory lockfile (<repo>/go/go.sum) neither loses
	// its directory nor collides with another repo's.
	FilesRoot string
	Env       []string // becomes ENV lines
	Tag       string
}

type Mount struct {
	Source   string // host path, or volume name when Volume
	Target   string // guest path
	Volume   bool
	ReadOnly bool
}

type RunSpec struct {
	Name    string // OrbName(session)
	Image   string
	Mounts  []Mount
	Env     []string
	Workdir string
	CPUs    int    // 0 = engine default
	Memory  string // "" = engine default, e.g. "8G"
}

type ExecOptions struct {
	Workdir     string
	Env         []string
	Interactive bool // attach stdin (-i)
}

type State string

const (
	StateMissing State = "missing"
	StateRunning State = "running"
	StateStopped State = "stopped"
)

const DefaultBase = "docker.io/library/debian:bookworm"

// ErrNotImplemented is what the Linux stubs return from every method.
var ErrNotImplemented = errors.New("container: runtime not implemented yet")

// Default picks the runtime for this OS: Apple on darwin; on linux the
// first of nerdctl/podman on PATH (stub); otherwise Unsupported.
func Default() Runtime {
	return pick(runtime.GOOS, exec.LookPath)
}

func pick(goos string, look func(string) (string, error)) Runtime {
	switch goos {
	case "darwin":
		return NewApple()
	case "linux":
		if _, err := look("nerdctl"); err == nil {
			return Nerdctl{}
		}
		if _, err := look("podman"); err == nil {
			return Podman{}
		}
	}
	return Unsupported{OS: goos}
}

// OrbName is the container name for a session.
func OrbName(session string) string { return "bough-orb-" + session }
