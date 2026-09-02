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
	"sort"
	"strconv"
	"strings"
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

// OpenExisting resumes an existing session JSONL: entries are loaded
// into memory, Seq continues at max+1, and the file is reopened for
// append — the log stays append-only. A corrupt line is skipped with a
// stderr note; a missing file is an error (resume must name a real
// session).
func OpenExisting(path string) (*Store, error) {
	entries, err := readEntries(path)
	if err != nil {
		return nil, fmt.Errorf("history: resume %s: %w", path, err)
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, fmt.Errorf("history: resume: %w", err)
	}
	var seq int64
	for _, e := range entries {
		if e.Seq > seq {
			seq = e.Seq
		}
	}
	return &Store{f: f, w: bufio.NewWriter(f), path: path, entries: entries, seq: seq}, nil
}

// readEntries parses a session JSONL, skipping corrupt lines with a
// stderr note.
func readEntries(path string) ([]Entry, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var entries []Entry
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	line := 0
	for sc.Scan() {
		line++
		var e Entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			fmt.Fprintf(os.Stderr, "bough: history: %s:%d: skipping corrupt line: %v\n", path, line, err)
			continue
		}
		entries = append(entries, e)
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	return entries, nil
}

// SessionInfo describes one stored session, for `bough sessions` and
// the ui session picker (the "sessions" service is []SessionInfo).
type SessionInfo struct {
	ID      string    // file base name without .jsonl
	Path    string    // full path
	ModTime time.Time // file mtime (last activity)
	Entries int       // parseable entry count
	Title   string    // first input entry's text, first line
}

// List scans dir for session JSONL files, newest first (mtime, then
// name descending). A nonexistent dir is an empty list, not an error.
// Corrupt lines within a file are tolerated (skipped with a stderr
// note by way of readEntries' counting pass here being lenient).
func List(dir string) ([]SessionInfo, error) {
	paths, err := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	if err != nil {
		return nil, fmt.Errorf("history: list %s: %w", dir, err)
	}
	var infos []SessionInfo
	for _, p := range paths {
		st, err := os.Stat(p)
		if err != nil {
			fmt.Fprintf(os.Stderr, "bough: history: skipping %s: %v\n", p, err)
			continue
		}
		entries, err := readEntries(p)
		if err != nil {
			fmt.Fprintf(os.Stderr, "bough: history: skipping %s: %v\n", p, err)
			continue
		}
		title := ""
		for _, e := range entries {
			if e.Kind == "input" {
				title, _ = e.Data["text"].(string)
				if i := strings.IndexByte(title, '\n'); i >= 0 {
					title = title[:i]
				}
				break
			}
		}
		infos = append(infos, SessionInfo{
			ID:      strings.TrimSuffix(filepath.Base(p), ".jsonl"),
			Path:    p,
			ModTime: st.ModTime(),
			Entries: len(entries),
			Title:   title,
		})
	}
	sort.Slice(infos, func(i, j int) bool {
		if !infos[i].ModTime.Equal(infos[j].ModTime) {
			return infos[i].ModTime.After(infos[j].ModTime)
		}
		return infos[i].ID > infos[j].ID
	})
	return infos, nil
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

// Apply mounts the "history" service. Config: {file: <path>} resumes
// that exact session file (append continues, entries preloaded);
// absent, a fresh session file is created under ~/.bough/history.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	var s *Store
	fresh := false
	if v, has := cfg["file"]; has {
		path, ok := v.(string)
		if !ok || path == "" {
			return fmt.Errorf("history: file must be a non-empty string, got %v", v)
		}
		var err error
		if s, err = OpenExisting(path); err != nil {
			return err
		}
	} else {
		fresh = true
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("history: home dir: %w", err)
		}
		name := time.Now().UTC().Format(time.RFC3339) + "-" + strconv.Itoa(os.Getpid()) + ".jsonl"
		if s, err = Open(filepath.Join(home, ".bough", "history", name)); err != nil {
			return err
		}
	}
	ctx.Provide("history", s)
	ctx.Effect(func() {
		if err := s.Close(); err != nil {
			fmt.Fprintf(os.Stderr, "bough: history close: %v\n", err)
		}
		// A fresh session that never got an entry (e.g. the row was
		// swapped to a resumed file by the session picker, or the user
		// quit immediately) leaves no empty stray in the listing.
		if fresh {
			if st, err := os.Stat(s.Path()); err == nil && st.Size() == 0 {
				_ = os.Remove(s.Path())
			}
		}
	})
	return nil
}
