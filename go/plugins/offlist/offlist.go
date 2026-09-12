// Package offlist reads ~/.bough/off.yml, the single file that turns individual
// discovered things (skills, hooks, rules, watchers, plugins) off. Everything
// bough discovers is on by default, so any trouble reading the file — missing,
// empty, malformed — means "nothing is off": failing the other way would
// silently disable the user's whole setup.
package offlist

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"gopkg.in/yaml.v3"
)

// file is the on-disk shape of off.yml.
type file struct {
	// "disabled", not "off": off is a YAML 1.1 boolean, so a writer has
	// to quote the key and the file comes out looking like `"off":` —
	// wrong-footing anyone editing it by hand. `off:` is still read, so
	// a file written before this rename keeps working.
	Disabled []string `yaml:"disabled"`
	Off      []string `yaml:"off,omitempty"`
}

// List is a snapshot of off.yml: the entries in file order, plus any error from
// reading it.
type List struct {
	entries []string
	index   map[string]bool
	err     error
}

// Path is where the off switch lives for a given bough home.
func Path(home string) string { return filepath.Join(home, "off.yml") }

func key(kind, id string) string { return kind + ":" + id }

// cache keeps the parsed list keyed by path, refreshed when the file's mtime or
// size changes. Callers ask on every skill scan and hook fire, so parsing per
// call would be wasteful.
var cache struct {
	sync.Mutex
	entries map[string]*cacheEntry
}

type cacheEntry struct {
	list  *List
	mtime time.Time
	size  int64
}

// Load returns the current off list for a bough home. It never returns nil.
func Load(home string) *List {
	path := Path(home)

	var mtime time.Time
	var size int64
	if st, err := os.Stat(path); err == nil {
		mtime, size = st.ModTime(), st.Size()
	}

	cache.Lock()
	defer cache.Unlock()
	if cache.entries == nil {
		cache.entries = map[string]*cacheEntry{}
	}
	if e, ok := cache.entries[path]; ok && e.mtime.Equal(mtime) && e.size == size {
		return e.list
	}
	l := parse(path)
	cache.entries[path] = &cacheEntry{list: l, mtime: mtime, size: size}
	return l
}

func parse(path string) *List {
	l := &List{index: map[string]bool{}}

	data, err := os.ReadFile(path)
	if err != nil {
		// A missing file is the normal case: nothing is off.
		if !errors.Is(err, fs.ErrNotExist) {
			l.err = err
		}
		return l
	}

	var f file
	if err := yaml.Unmarshal(data, &f); err != nil {
		l.err = fmt.Errorf("%s: %w", path, err)
		return l
	}
	for _, e := range append(append([]string{}, f.Disabled...), f.Off...) {
		e = strings.TrimSpace(e)
		if e == "" || !strings.Contains(e, ":") {
			continue
		}
		if !l.index[e] {
			l.index[e] = true
			l.entries = append(l.entries, e)
		}
	}
	return l
}

// Off reports whether "<kind>:<id>" is listed.
func (l *List) Off(kind, id string) bool {
	if l == nil {
		return false
	}
	return l.index[key(kind, id)]
}

// Entries returns the listed "<kind>:<id>" strings in file order.
func (l *List) Entries() []string {
	if l == nil {
		return nil
	}
	return append([]string(nil), l.entries...)
}

// Err reports a malformed or unreadable off.yml. The list itself is still
// usable and empty.
func (l *List) Err() error {
	if l == nil {
		return nil
	}
	return l.err
}

// Set turns one item off or on and rewrites off.yml, preserving the order of
// the entries already there. It reads the file fresh rather than trusting the
// receiver's snapshot, and refuses to rewrite a file it could not parse — that
// would throw away entries the user wrote.
func (l *List) Set(home, kind, id string, off bool) error {
	path := Path(home)
	cur := parse(path)
	if err := cur.Err(); err != nil {
		return err
	}

	k := key(kind, id)
	entries := cur.entries
	if off {
		if cur.index[k] {
			return nil
		}
		entries = append(entries, k)
	} else {
		if !cur.index[k] {
			return nil
		}
		kept := make([]string, 0, len(entries))
		for _, e := range entries {
			if e != k {
				kept = append(kept, e)
			}
		}
		entries = kept
	}

	data, err := yaml.Marshal(file{Disabled: entries})
	if err != nil {
		return err
	}
	if err := os.MkdirAll(home, 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return err
	}

	l.entries = entries
	l.index = map[string]bool{}
	for _, e := range entries {
		l.index[e] = true
	}
	l.err = nil

	cache.Lock()
	delete(cache.entries, path)
	cache.Unlock()
	return nil
}
