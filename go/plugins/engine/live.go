//go:build !windows

package engine

import (
	"sync"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// liveHistory is the history service a Runtime writes to. The Runtime
// outlives engine-row remounts, but the history row hands out a new
// Store (same file) each time it remounts and closes the old one, so
// the session must always reach the current value, not the one it was
// opened with.
type liveHistory struct {
	mu sync.RWMutex
	h  loop.History
}

func (l *liveHistory) set(h loop.History) {
	l.mu.Lock()
	l.h = h
	l.mu.Unlock()
}

func (l *liveHistory) get() loop.History {
	l.mu.RLock()
	defer l.mu.RUnlock()
	return l.h
}

func (l *liveHistory) Append(kind string, data map[string]any) history.Entry {
	return l.get().Append(kind, data)
}

func (l *liveHistory) Entries() []history.Entry { return l.get().Entries() }
func (l *liveHistory) Path() string             { return l.get().Path() }

// liveRegistry is the agent-tools registry a Runtime snapshots. A
// remount of the agent-tools row hands out a new, empty registry that
// the tool rows then re-register into; the session sees that as one
// more tool-set change (a restart at idle), never as a registry it
// holds going quiet.
type liveRegistry struct {
	mu   sync.Mutex
	cur  agenttools.Registry
	ch   chan struct{}
	stop chan struct{}
}

func newLiveRegistry(r agenttools.Registry) *liveRegistry {
	l := &liveRegistry{ch: make(chan struct{})}
	l.set(r)
	return l
}

// set swaps the registry in; a different one counts as a change.
func (l *liveRegistry) set(r agenttools.Registry) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.cur == r {
		return
	}
	first := l.cur == nil
	if l.stop != nil {
		close(l.stop)
	}
	l.cur, l.stop = r, make(chan struct{})
	go l.forward(r, l.stop)
	if !first {
		l.fireLocked()
	}
}

// forward relays r's changes until r is swapped out or the row closes.
func (l *liveRegistry) forward(r agenttools.Registry, stop <-chan struct{}) {
	for {
		select {
		case <-r.Changed():
			l.mu.Lock()
			l.fireLocked()
			l.mu.Unlock()
		case <-stop:
			return
		}
	}
}

func (l *liveRegistry) close() {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.stop != nil {
		close(l.stop)
		l.stop = nil
	}
}

func (l *liveRegistry) fireLocked() {
	close(l.ch)
	l.ch = make(chan struct{})
}

func (l *liveRegistry) get() agenttools.Registry {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.cur
}

func (l *liveRegistry) Register(t agenttools.Tool) (func(), error) { return l.get().Register(t) }
func (l *liveRegistry) Lookup(name string) (agenttools.Tool, bool) { return l.get().Lookup(name) }
func (l *liveRegistry) Tools() []agenttools.Tool                   { return l.get().Tools() }

func (l *liveRegistry) Changed() <-chan struct{} {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.ch
}
