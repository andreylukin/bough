//go:build !windows

// Package ops is the session-lifetime operation manager every
// coordinator of one bough session shares (go/docs/unreal-engine.md
// §5.5).
//
// A coordinator is rebuilt many times in one session: at a tool-set
// change, after Run dies, after a history switch. The operations it
// started keep running across those rebuilds, so they belong to a
// manager that outlives any one coordinator. Two harness behaviours make
// a plain LocalOperationManager unsafe to share that way, both verified
// in the pin:
//   - LocalOperationManager.Add of an id it already accepted returns nil
//     and emits nothing (harness/operation/local_manager.go:133);
//   - the coordinator's slurpChannel returns on ctx.Done after it has
//     already dequeued updates (harness/coordinator/loop.go:198, :438).
//
// Together: a terminal update the dying coordinator took off the
// channel but never saved is lost for good, and the next coordinator
// re-Adds the op from the store, gets nothing, and waits forever. So
// this manager remembers the latest snapshot of every op and re-emits it
// when a known id is Added again.
package ops

import (
	"context"
	"sync"

	"github.com/unreallabsai/unreal-agent/harness/operation"
)

// Manager is the session-lifetime operation.Manager every coordinator of
// one session shares.
type Manager struct {
	ctx   context.Context
	inner *operation.LocalOperationManager
	tap   func(operation.Operation)

	mu      sync.Mutex
	latest  map[operation.ID]operation.Operation
	updated map[operation.ID]bool // latest came from an update, not only the Add
	queue   []operation.ID        // first-enqueue order
	pending map[operation.ID]operation.Operation
	version map[operation.ID]int // bumped on every enqueue: feed can tell its head was replaced
	kick    chan struct{}
	out     chan operation.Operation
}

var _ operation.Manager = (*Manager)(nil)

// New wraps inner, which must live on ctx. tap (may be nil) sees every
// update inner produces, before it is queued.
func New(ctx context.Context, inner *operation.LocalOperationManager, tap func(operation.Operation)) *Manager {
	m := &Manager{
		ctx:     ctx,
		inner:   inner,
		tap:     tap,
		latest:  map[operation.ID]operation.Operation{},
		updated: map[operation.ID]bool{},
		pending: map[operation.ID]operation.Operation{},
		version: map[operation.ID]int{},
		kick:    make(chan struct{}, 1),
		out:     make(chan operation.Operation),
	}
	go m.pump()
	go m.feed()
	return m
}

// Add starts op, or, for an id this manager already knows, re-emits the
// latest snapshot it holds unless one is already queued: the caller is a
// new coordinator restoring from a store that may be behind.
func (m *Manager) Add(op operation.Operation) error {
	m.mu.Lock()
	if _, known := m.latest[op.ID]; known {
		if m.updated[op.ID] {
			m.enqueueLocked(m.latest[op.ID])
		}
		m.mu.Unlock()
		return nil
	}
	m.latest[op.ID] = clone(op)
	m.mu.Unlock()
	if err := m.inner.Add(op); err != nil {
		m.mu.Lock()
		delete(m.latest, op.ID)
		m.mu.Unlock()
		return err
	}
	return nil
}

func (m *Manager) Cancel(id operation.ID, reason string) error {
	return m.inner.Cancel(id, reason)
}

// Updates is one channel for the whole session: a coordinator that dies
// leaves what it did not read queued for the next. It is closed only
// when the session ctx ends.
func (m *Manager) Updates() <-chan operation.Operation { return m.out }

// Latest is the newest snapshot of op id this manager has seen.
func (m *Manager) Latest(id operation.ID) (operation.Operation, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	op, ok := m.latest[id]
	return clone(op), ok
}

// enqueueLocked queues a snapshot: at most one per id, the newest wins
// and keeps the place of the first. A re-Add replay and an update still
// queued therefore coalesce instead of arriving twice.
func (m *Manager) enqueueLocked(op operation.Operation) {
	if _, queued := m.pending[op.ID]; !queued {
		m.queue = append(m.queue, op.ID)
	}
	m.pending[op.ID] = clone(op)
	m.version[op.ID]++
	select {
	case m.kick <- struct{}{}:
	default:
	}
}

func (m *Manager) pump() {
	updates := m.inner.Updates()
	for {
		select {
		case op, ok := <-updates:
			if !ok {
				return
			}
			if m.tap != nil {
				m.tap(clone(op))
			}
			m.mu.Lock()
			m.latest[op.ID] = clone(op)
			m.updated[op.ID] = true
			m.enqueueLocked(op)
			m.mu.Unlock()
		case <-m.ctx.Done():
			return
		}
	}
}

// feed hands the queue's head to whoever reads Updates. The head stays
// queued until it is taken, so a replay or a newer update that arrives
// while nobody is reading replaces it instead of following it.
func (m *Manager) feed() {
	defer close(m.out)
	for {
		m.mu.Lock()
		var next operation.Operation
		var id operation.ID
		ver := 0
		have := len(m.queue) > 0
		if have {
			id = m.queue[0]
			next, ver = m.pending[id], m.version[id]
		}
		m.mu.Unlock()
		if !have {
			select {
			case <-m.kick:
				continue
			case <-m.ctx.Done():
				return
			}
		}
		select {
		case m.out <- next:
			m.mu.Lock()
			if m.version[id] == ver {
				m.queue = m.queue[1:]
				delete(m.pending, id)
				delete(m.version, id)
			}
			m.mu.Unlock()
		case <-m.kick:
		case <-m.ctx.Done():
			return
		}
	}
}

func clone(op operation.Operation) operation.Operation {
	op.State = op.State.Clone()
	op.Idempotency = op.Idempotency.Clone()
	return op
}
