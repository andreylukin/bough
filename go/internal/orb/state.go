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
	UpdatedAt time.Time         `json:"updatedAt"`
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
