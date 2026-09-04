// Package history is the "history" plugin: an append-only, inspectable
// JSONL record of the session. One file per session under
// ~/.bough/history/<uuidv7>.jsonl; entries are never rewritten.
// It provides the "history" service the loop appends every turn to.
package history

import (
	"bufio"
	"cmp"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/google/uuid"
)

// Entry is one history record. At marshals as RFC3339Nano. Parent is
// the seq this entry follows (the session tree, see Ancestors); absent
// (0) it defaults to the previous seq — use ParentOf.
type Entry struct {
	Seq    int64          `json:"seq"`
	At     time.Time      `json:"at"`
	Kind   string         `json:"kind"`
	Data   map[string]any `json:"data,omitempty"`
	Parent int64          `json:"parent,omitempty"`
}

// ParentOf is the seq e follows: its parent field, else the previous
// seq (0 for the first entry).
func ParentOf(e Entry) int64 {
	if e.Parent != 0 {
		return e.Parent
	}
	return e.Seq - 1
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

// NewID is a session id: a UUIDv7, so it is unique per session (two
// bough processes starting in the same second used to be able to
// collide on the old <RFC3339>-<pid> name once the pid wrapped) while
// still sorting oldest-first by its leading millisecond timestamp.
// A failed random read falls back to a timestamp-and-pid name rather
// than refusing to record the session.
func NewID() string {
	if id, err := uuid.NewV7(); err == nil {
		return id.String()
	}
	return time.Now().UTC().Format("20060102T150405.000Z") + "-" + strconv.Itoa(os.Getpid())
}

// SessionInfo describes one stored session, for `bough sessions` and
// the ui session picker (the "sessions" service is []SessionInfo).
type SessionInfo struct {
	ID      string    // file base name without .jsonl (a UUIDv7 for new sessions)
	Path    string    // full path
	ModTime time.Time // file mtime (last activity)
	Entries int       // parseable entry count
	Title   string    // first input entry's text, first line
	Cwd     string    // working directory from the "meta" entry; "" for old files
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
		title, cwd := "", ""
		for _, e := range entries {
			// A "title" entry is a name the session was given (the
			// session-title plugin); it wins over the opening line.
			if e.Kind == "title" {
				if t, _ := e.Data["text"].(string); t != "" {
					title = t
					continue
				}
			}
			if e.Kind == "meta" && cwd == "" {
				cwd, _ = e.Data["cwd"].(string)
			}
			if e.Kind == "input" && title == "" {
				title, _ = e.Data["text"].(string)
				if i := strings.IndexByte(title, '\n'); i >= 0 {
					title = title[:i]
				}
			}
		}
		infos = append(infos, SessionInfo{
			ID:      strings.TrimSuffix(filepath.Base(p), ".jsonl"),
			Path:    p,
			ModTime: st.ModTime(),
			Entries: len(entries),
			Title:   title,
			Cwd:     cwd,
		})
	}
	slices.SortFunc(infos, func(a, b SessionInfo) int {
		return cmp.Or(b.ModTime.Compare(a.ModTime), cmp.Compare(b.ID, a.ID))
	})
	return infos, nil
}

// PreferCwd reorders infos (stably) so sessions recorded in cwd come
// first: sessions are global across projects, so every listing and
// -c lead with this directory's.
func PreferCwd(infos []SessionInfo, cwd string) []SessionInfo {
	out := slices.Clone(infos)
	slices.SortStableFunc(out, func(a, b SessionInfo) int {
		if x, y := a.Cwd == cwd, b.Cwd == cwd; x != y {
			if x {
				return -1
			}
			return 1
		}
		return 0
	})
	return out
}

// LastPrompt is the first line of the last "input" entry, "" if none.
func LastPrompt(entries []Entry) string {
	for _, e := range slices.Backward(entries) {
		if e.Kind == "input" {
			text, _ := e.Data["text"].(string)
			return strings.SplitN(text, "\n", 2)[0]
		}
	}
	return ""
}

// Append records one entry: monotonically increasing Seq, current time,
// one JSON line flushed to disk. A write error is loud on stderr but
// the in-memory entry survives, so the session keeps working.
func (s *Store) Append(kind string, data map[string]any) Entry {
	s.mu.Lock()
	defer s.mu.Unlock()
	e := Entry{Seq: s.seq + 1, At: time.Now(), Kind: kind, Data: data, Parent: s.seq}
	s.seq++
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

// onlyMeta reports whether nothing but the "meta" entry was recorded.
func (s *Store) onlyMeta() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, e := range s.entries {
		if e.Kind != "meta" {
			return false
		}
	}
	return true
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
// that exact session file (append continues, entries preloaded) — a
// missing file is created; absent, a fresh session file is created
// under ~/.bough/history. A created file opens with a "meta" entry
// recording the working directory (SessionInfo.Cwd).
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	var s *Store
	fresh := false // auto-named: removed at close if nothing but meta landed
	created := false
	if v, has := cfg["file"]; has {
		path, ok := v.(string)
		if !ok || path == "" {
			return fmt.Errorf("history: file must be a non-empty string, got %v", v)
		}
		var err error
		if _, serr := os.Stat(path); errors.Is(serr, fs.ErrNotExist) {
			created = true
			s, err = Open(path)
		} else {
			s, err = OpenExisting(path)
		}
		if err != nil {
			return err
		}
	} else {
		fresh, created = true, true
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("history: home dir: %w", err)
		}
		name := NewID() + ".jsonl"
		if s, err = Open(filepath.Join(home, ".bough", "history", name)); err != nil {
			return err
		}
	}
	if created {
		if cwd, err := os.Getwd(); err == nil {
			s.Append("meta", map[string]any{"cwd": cwd})
		}
	}
	ctx.Provide("history", s)
	ctx.Provide("checkpoints", &Checkpoints{session: strings.TrimSuffix(filepath.Base(s.Path()), ".jsonl")})
	ctx.Effect(func() {
		if err := s.Close(); err != nil {
			fmt.Fprintf(os.Stderr, "bough: history close: %v\n", err)
		}
		// A fresh session that never got an entry (e.g. the row was
		// swapped to a resumed file by the session picker, or the user
		// quit immediately) leaves no stray in the listing.
		if fresh && s.onlyMeta() {
			_ = os.Remove(s.Path())
		}
	})
	return nil
}

// RecentPrompts is what the composer's Up arrow recalls: the prompts
// typed in this directory before now, oldest last (index 0 is the most
// recent), consecutive duplicates squeezed.
//
// Prompt history has to outlive the session or it is not history at
// all — a bough started in a directory it has been used in all week
// opened with an empty Up arrow, and it got worse when every launch
// started a new session by default. Sessions from other directories
// are skipped: recalling another project's prompts is noise.
//
// Files are read newest-first and the scan stops as soon as limit
// prompts are in hand, so the common case touches a handful of the
// hundreds of session files rather than parsing all of them the way
// List does.
func RecentPrompts(dir, cwd string, limit int) []string {
	paths, err := filepath.Glob(filepath.Join(dir, "*.jsonl"))
	if err != nil || len(paths) == 0 {
		return nil
	}
	type stamped struct {
		path string
		mod  time.Time
	}
	files := make([]stamped, 0, len(paths))
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil {
			files = append(files, stamped{p, st.ModTime()})
		}
	}
	slices.SortFunc(files, func(a, b stamped) int {
		return cmp.Or(b.mod.Compare(a.mod), cmp.Compare(b.path, a.path))
	})
	var out []string
	for _, f := range files {
		prompts, ok := filePrompts(f.path, cwd)
		if !ok {
			continue
		}
		// Within a file the prompts are oldest-first; the newest file
		// comes first overall, so each file's prompts are prepended in
		// reverse to keep "most recent" at index 0.
		for i := len(prompts) - 1; i >= 0; i-- {
			if len(out) > 0 && out[len(out)-1] == prompts[i] {
				continue // consecutive duplicate
			}
			out = append(out, prompts[i])
			if len(out) >= limit {
				return out
			}
		}
	}
	return out
}

// filePrompts reads one session file's input texts in order. ok is
// false when the session belongs to another directory — decided from
// the "meta" entry, which is the first line, so a foreign session
// costs one line rather than a full parse. A file with no meta (an old
// one) is accepted: its prompts are better than none.
func filePrompts(path, cwd string) (prompts []string, ok bool) {
	f, err := os.Open(path)
	if err != nil {
		return nil, false
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		var e Entry
		if json.Unmarshal(sc.Bytes(), &e) != nil {
			continue
		}
		switch e.Kind {
		case "meta":
			if c, _ := e.Data["cwd"].(string); c != "" && cwd != "" && c != cwd {
				return nil, false
			}
		case "input":
			// "typed" is the raw line when the message sent differs
			// from it (@file expansion, an injected skill); recalling
			// the expansion would put a file's contents in the
			// composer.
			t, _ := e.Data["typed"].(string)
			if t == "" {
				t, _ = e.Data["text"].(string)
			}
			// A background job waking an idle agent is written as an
			// input, but the user never typed it.
			if strings.TrimSpace(t) == "" || strings.HasPrefix(t, "[background job] ") {
				continue
			}
			prompts = append(prompts, t)
		}
	}
	return prompts, true
}
