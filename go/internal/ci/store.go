package ci

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Result is one run of one check on one key.
type Result struct {
	Check      string    `json:"check"`
	Key        string    `json:"key"`
	Tree       string    `json:"tree"`   // the tree it ran on; another tree may share the key
	Status     string    `json:"status"` // pass | fail
	ExitCode   int       `json:"exit_code"`
	Started    time.Time `json:"started"`
	Finished   time.Time `json:"finished"`
	DurationMS int64     `json:"duration_ms"`
}

// Store is one repo's content-addressed results:
// <state>/results/<check>/<key>.json with <key>.log beside it.
type Store struct {
	Home string
	Repo Repo
}

func (s *Store) dir() string { return StateDir(s.Home, s.Repo) }

func (s *Store) resultPath(check, key string) string {
	return filepath.Join(s.dir(), "results", check, key+".json")
}

// LogPath is where the output of check's run under key is kept.
func (s *Store) LogPath(check, key string) string {
	return filepath.Join(s.dir(), "results", check, key+".log")
}

// Lookup returns the stored result, or nil when the key has none.
func (s *Store) Lookup(check, key string) (*Result, error) {
	b, err := os.ReadFile(s.resultPath(check, key))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("ci: read result %s/%s: %w", check, key[:12], err)
	}
	var r Result
	if err := json.Unmarshal(b, &r); err != nil {
		// A torn file cannot happen (rename), but a hand-edited one can:
		// treat it as absent so the check simply runs again.
		return nil, nil
	}
	return &r, nil
}

// save writes the log, then the result. The json is what marks a check
// settled, so it lands last and by rename: a reader never sees a result
// whose log is missing or half written.
func (s *Store) save(res Result, log []byte) error {
	dir := filepath.Dir(s.resultPath(res.Check, res.Key))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("ci: results dir: %w", err)
	}
	if err := writeAtomic(s.LogPath(res.Check, res.Key), log); err != nil {
		return fmt.Errorf("ci: save log of %s: %w", res.Check, err)
	}
	b, err := json.MarshalIndent(res, "", "  ")
	if err != nil {
		return err
	}
	if err := writeAtomic(s.resultPath(res.Check, res.Key), append(b, '\n')); err != nil {
		return fmt.Errorf("ci: save result of %s: %w", res.Check, err)
	}
	return nil
}

func writeAtomic(path string, b []byte) error {
	tmp, err := os.CreateTemp(filepath.Dir(path), ".tmp-*")
	if err != nil {
		return err
	}
	if _, err := tmp.Write(b); err != nil {
		tmp.Close()
		os.Remove(tmp.Name())
		return err
	}
	if err := tmp.Close(); err != nil {
		os.Remove(tmp.Name())
		return err
	}
	if err := os.Rename(tmp.Name(), path); err != nil {
		os.Remove(tmp.Name())
		return err
	}
	return nil
}
