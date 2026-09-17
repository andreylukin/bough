// Package orb is the lifecycle of a project session's container: the
// snapshot image, the session's git worktrees, the long-lived container
// and the exec seam tools run through. The child process that owns the
// session is the only writer of its state.json; serve only reads it.
package orb

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

type Status string

// What a failed orb failed at, so a ui offers the fix that applies:
// rebuild the image, edit resume.sh, or fix the definition/runtime.
const (
	// PhaseBuild ("build"): the image build (setup.sh / Dockerfile)
	PhaseSetup = "setup" // resume.sh in the running container
	PhaseStart = "start" // runtime, repos, worktrees or the container start
)

// FailedAt is what a failed orb failed at: PhaseBuild, PhaseSetup or
// PhaseStart, derived from the step it stopped in. "" unless failed.
func FailedAt(s State) string {
	if s.Status != StatusFailed {
		return ""
	}
	switch s.Phase {
	case PhaseBuild:
		return PhaseBuild
	case PhaseResume:
		return PhaseSetup
	}
	return PhaseStart
}

const (
	StatusNone     Status = ""         // local session
	StatusBuilding Status = "building" // image build in progress
	StatusStarting Status = "starting" // worktrees/container/resume
	StatusRunning  Status = "running"
	StatusStopped  Status = "stopped"
	StatusFailed   Status = "failed"
)

// State is ~/.bough/orbs/<session>/state.json.
type State struct {
	Session   string            `json:"session"`
	Project   string            `json:"project"` // slug
	Status    Status            `json:"status"`
	Image     string            `json:"image,omitempty"`
	Container string            `json:"container,omitempty"`
	Worktrees map[string]string `json:"worktrees,omitempty"` // repo name -> host path
	Primary   string            `json:"primary,omitempty"`
	Error     string            `json:"error,omitempty"`
	PID       int               `json:"pid,omitempty"`
	ProxyAuth string            `json:"proxyAuth,omitempty"` // ProxyAuthToken or ProxyAuthLegacy
	Phase     string            `json:"phase,omitempty"`     // the step in progress, or the last one reached
	Phases    []Phase           `json:"phases,omitempty"`    // this start's steps, in order
	UpdatedAt time.Time         `json:"updatedAt"`
}

// The named steps of a start, in order. A restart after Stop records
// only container, resume.sh and ready.
const (
	PhaseSync      = "sync"      // clone or fetch remote repos
	PhaseBuild     = "build"     // ensure the snapshot image
	PhaseWorktree  = "worktree"  // the session's git worktrees and mounts
	PhaseContainer = "container" // create or start the container
	PhaseResume    = "resume.sh" // proxy, relay shim and resume.sh
	PhaseReady     = "ready"
)

// Phase is one timed step. EndedAt is zero while it runs; Error is set
// on the step a start failed in.
type Phase struct {
	Name      string    `json:"name"`
	StartedAt time.Time `json:"startedAt"`
	EndedAt   time.Time `json:"endedAt,omitzero"`
	Error     string    `json:"error,omitempty"`
}

var phaseWords = map[string]string{
	PhaseSync: "sync repos", PhaseBuild: "build image", PhaseWorktree: "worktree",
	PhaseContainer: "start container", PhaseResume: "resume.sh", PhaseReady: "ready",
}

// begin ends the running step and starts name; ready ends at once.
func (s *State) begin(name string) {
	now := time.Now().UTC()
	s.endPhase("")
	ph := Phase{Name: name, StartedAt: now}
	if name == PhaseReady {
		ph.EndedAt = now
	}
	s.Phase = name
	s.Phases = append(s.Phases, ph)
}

// endPhase closes the running step, recording why it failed when errMsg
// is set.
func (s *State) endPhase(errMsg string) {
	if n := len(s.Phases); n > 0 && s.Phases[n-1].EndedAt.IsZero() {
		s.Phases[n-1].EndedAt = time.Now().UTC()
		s.Phases[n-1].Error = errMsg
	}
}

// PhaseWord names a phase for people ("build image").
func PhaseWord(name string) string {
	if w, ok := phaseWords[name]; ok {
		return w
	}
	return name
}

// PhaseLine is the one-line orb status: the running step with its
// elapsed time while a start is under way, else the orb's state.
func PhaseLine(s State, now time.Time) string {
	head := "orb " + s.Project + " · "
	switch s.Status {
	case StatusRunning, StatusStopped:
		return head + string(s.Status)
	case StatusFailed:
		if s.Phase != "" {
			return head + "failed at " + s.Phase
		}
		return head + "failed"
	}
	if n := len(s.Phases); n > 0 && s.Phases[n-1].EndedAt.IsZero() {
		ph := s.Phases[n-1]
		return fmt.Sprintf("%s%s %s", head, PhaseWord(ph.Name), now.Sub(ph.StartedAt).Truncate(time.Second))
	}
	return head + "starting"
}

const stateFile = "state.json"

func orbsRoot(home string) string { return filepath.Join(home, ".bough", "orbs") }

func Dir(home, session string) string { return filepath.Join(orbsRoot(home), session) }

func ReadState(home, session string) (State, error) {
	var s State
	err := readJSON(filepath.Join(Dir(home, session), stateFile), &s)
	if err != nil {
		return State{}, fmt.Errorf("orb: read state %s: %w", session, err)
	}
	return s, nil
}

func writeState(home string, s State) error {
	s.UpdatedAt = time.Now().UTC()
	if err := writeJSON(filepath.Join(Dir(home, s.Session), stateFile), s); err != nil {
		return fmt.Errorf("orb: write state %s: %w", s.Session, err)
	}
	return nil
}

// MarkStopped rewrites a session's state.json as stopped for a stop made
// outside the owning child (serve's Stop orb, archive, kill), so every
// reader of the file sees it; the child's next restart writes running
// again. It returns the state it replaced (zero when there was none).
func MarkStopped(home, session string) (State, error) {
	prev, err := ReadState(home, session)
	if err != nil || prev.Session == "" {
		return prev, err
	}
	next := prev
	next.Status = StatusStopped
	return prev, writeState(home, next)
}

// Restore puts back a state MarkStopped replaced (a stop that failed).
func Restore(home string, prev State) error {
	if prev.Session == "" {
		return nil
	}
	return writeJSON(filepath.Join(Dir(home, prev.Session), stateFile), prev)
}

// readJSON leaves v untouched when the file is missing.
func readJSON(path string, v any) error {
	b, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	return json.Unmarshal(b, v)
}

// writeJSON is temp+rename so serve never reads a half-written file.
func writeJSON(path string, v any) error {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(path), "."+filepath.Base(path)+".tmp-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if _, err := f.Write(b); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(f.Name(), path)
}
