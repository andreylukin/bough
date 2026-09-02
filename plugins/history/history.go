// Package history is the "history" plugin: an append-only, inspectable
// JSONL record of the session. One file per session under
// ~/.bough/history/<RFC3339 ts>-<pid>.jsonl; entries are never rewritten.
// It provides the "history" service the loop appends every turn to.
package history

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// Entry is one history record. At marshals as RFC3339Nano.
type Entry struct {
	Seq  int64          `json:"seq"`
	At   time.Time      `json:"at"`
	Kind string         `json:"kind"`
	Data map[string]any `json:"data,omitempty"`
}

// Store is an append-only JSONL session log. It implements the
// "history" service contract used by the loop.
type Store struct {
	mu      sync.Mutex
	f       *os.File
	w       *bufio.Writer
	path    string
	entries []Entry
	seq     int64
}

// Open creates (or truncates) the JSONL file at path, creating parent
// directories as needed.
func Open(path string) (*Store, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, fmt.Errorf("history: %w", err)
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, fmt.Errorf("history: %w", err)
	}
	return &Store{f: f, w: bufio.NewWriter(f), path: path}, nil
}

// Append records one entry: monotonically increasing Seq, current time,
// one JSON line flushed to disk. A write error is loud on stderr but
// the in-memory entry survives, so the session keeps working.
func (s *Store) Append(kind string, data map[string]any) Entry {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.seq++
	e := Entry{Seq: s.seq, At: time.Now(), Kind: kind, Data: data}
	s.entries = append(s.entries, e)
	line, err := json.Marshal(e)
	if err == nil {
		_, err = s.w.Write(append(line, '\n'))
	}
	if err == nil {
		err = s.w.Flush()
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "bough: history append: %v\n", err)
	}
	return e
}

// Entries returns a copy of this session's entries.
func (s *Store) Entries() []Entry {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]Entry(nil), s.entries...)
}

// Path returns the JSONL file path.
func (s *Store) Path() string { return s.path }

// Close flushes and closes the file.
func (s *Store) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.w.Flush(); err != nil {
		return err
	}
	return s.f.Close()
}

type plugin struct{}

func init() {
	kernel.Register("history", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "history" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("history: home dir: %w", err)
	}
	name := time.Now().UTC().Format(time.RFC3339) + "-" + strconv.Itoa(os.Getpid()) + ".jsonl"
	s, err := Open(filepath.Join(home, ".bough", "history", name))
	if err != nil {
		return err
	}
	ctx.Provide("history", s)
	ctx.Effect(func() {
		if err := s.Close(); err != nil {
			fmt.Fprintf(os.Stderr, "bough: history close: %v\n", err)
		}
	})
	return nil
}
