// Package container is the orb runtime seam: a small interface over a
// container engine CLI, so project sessions can build a snapshot image,
// keep one long-lived container per session and exec commands in it.
// Backends shell out to their CLI; no cgo, no daemon SDKs.
package container

import (
	"context"
	"errors"
	"io"
	"os"
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
	// Running names every running container in one call: the control
	// room asks about every orb on every list, and one exec is what a
	// list can afford where sixty are not.
	Running(ctx context.Context) ([]string, error)
	CreateVolume(ctx context.Context, name string) error // exists is nil
	// Images lists every image tag; ContainerImages the image of every
	// container, stopped ones included; RemoveImage deletes one tag.
	Images(ctx context.Context) ([]string, error)
	ContainerImages(ctx context.Context) ([]string, error)
	RemoveImage(ctx context.Context, tag string) error
}

type BuildSpec struct {
	Dir        string // build context
	Dockerfile string // path, usually Dir/Dockerfile
	Tag        string
}

// CommitSpec is a setup-script build: one COPY+RUN layer per step, so
// BuildKit's layer cache re-runs only the first changed step onward.
type CommitSpec struct {
	Base  string // generic base image, DefaultBase when empty
	Steps []Step
	// FilesRoot is the host dir every Step.Files path lies under; each
	// file lands at /bough-setup/lock/<path relative to it>.
	FilesRoot string
	Tag       string
}

// Step is one layer: its declared files and script are copied in right
// before its RUN. There are no ENV lines: project env reaches the
// container at run and exec time, never the image.
type Step struct {
	Name   string // file-name safe; the script is steps/<nn>-<name>.sh
	Script []byte // run with ScriptArgv
	Files  []string
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
	CPUs    int       // 0 = engine default
	Memory  string    // "" = engine default, e.g. "8G"
	Ports   []PortMap // published on 127.0.0.1 only; fixed at create
}

// PortMap forwards 127.0.0.1:Host to Guest in the container.
type PortMap struct{ Host, Guest int }

// Addresser is a runtime that knows a running container's IP on its
// bridge network ("" when it has none, e.g. stopped).
type Addresser interface {
	Address(ctx context.Context, name string) (string, error)
}

type ExecOptions struct {
	Workdir string
	Env     []string
	// Secrets are NAME=value pairs kept out of argv: the exec client gets
	// only -e NAME and inherits the value from its own environment.
	Secrets     []string
	Interactive bool // attach stdin (-i)
}

type State string

const (
	StateMissing State = "missing"
	StateRunning State = "running"
	StateStopped State = "stopped"
)

const DefaultBase = "docker.io/library/debian:bookworm"

// ErrNotImplemented is what the nerdctl stub returns from every method.
var ErrNotImplemented = errors.New("container: runtime not implemented yet")

// Default picks the runtime for this OS: Apple on darwin; on linux
// podman when on PATH, else the nerdctl stub; otherwise Unsupported.
//
// BOUGH_CONTAINER=none picks Unsupported everywhere. The serve-level
// test suites run the real binary, and without it a project listing
// asks the host's engine about images: on a laptop that is the
// person's own containers answering a test.
func Default() Runtime {
	return pick(runtime.GOOS, exec.LookPath, os.Getenv("BOUGH_CONTAINER"))
}

func pick(goos string, look func(string) (string, error), engine string) Runtime {
	if engine == "none" {
		return Unsupported{OS: goos + " (BOUGH_CONTAINER=none)"}
	}
	switch goos {
	case "darwin":
		return NewApple()
	case "linux":
		if _, err := look("podman"); err == nil {
			return NewPodman()
		}
		if _, err := look("nerdctl"); err == nil {
			return Nerdctl{}
		}
	}
	return Unsupported{OS: goos}
}

// OrbName is the container name for a session.
func OrbName(session string) string { return "bough-orb-" + session }
