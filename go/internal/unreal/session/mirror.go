package session

import (
	"sync"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
)

// Mirror is the coordinator's scheduling state rebuilt from the items it
// records (§9.1). It encodes the coordinator's own rule: it calls the
// model iff something is pending and no request is in flight, so a
// mirror with Pending == 0, Inflight false and no running call means the
// coordinator has nothing left to do. Apply is O(1), pure and does no
// I/O: one replica runs inside the store observer for the Gate, another
// on the actor for the done rule, and both are seeded from the store at
// Open because the observer does not fire during a restore.
type Mirror struct {
	Pending     int
	Reasons     []string // what is pending: "input:<id>", "call:<id>", "heartbeat:<id>"
	TurnReasons []string // what the newest request answers
	LastTurn    session.TurnID
	Inflight    bool
	Outstanding map[string]*Call // call id → a call whose result the model has not had yet
}

// Call is one outstanding tool call.
type Call struct {
	ID      string
	Tool    string
	Args    string
	Turn    session.TurnID
	FirstAt time.Time
	Ops     []operation.ID
}

func newMirror() *Mirror { return &Mirror{Outstanding: map[string]*Call{}} }

// Apply folds one store item in.
func (m *Mirror) Apply(it sessionstore.Item) {
	switch it.Kind {
	case sessionstore.ItemFork:
		// The coordinator drops inherited calls at a fork (its FIXME);
		// pending-input accounting carries over, and so does ours.
		clear(m.Outstanding)
	case sessionstore.ItemInput:
		in, _ := it.Data.(inbox.Input)
		switch in.Kind {
		case inbox.InputExternal:
			m.Pending++
			m.Reasons = append(m.Reasons, "input:"+string(in.ID))
		case inbox.InputControl:
			if msg, err := in.DecodeControlMessage(); err == nil && msg.Mode == inbox.Heartbeat {
				m.Pending++
				m.Reasons = append(m.Reasons, "heartbeat:"+string(in.ID))
			}
		}
	case sessionstore.ItemTurn:
		t, _ := it.Data.(session.Turn)
		m.LastTurn = t.ID
		m.Inflight = true
		m.TurnReasons = m.Reasons
		m.Reasons = nil
		m.Pending = 0
	case sessionstore.ItemModelResponse:
		mr, _ := it.Data.(sessionstore.ModelResponse)
		if mr.TurnID != m.LastTurn {
			return
		}
		m.Inflight = false
		for _, o := range mr.Response.Output {
			tc, ok := o.Data.(ullm.ToolCall)
			if o.Type != ullm.ItemToolCall || !ok {
				continue
			}
			m.Outstanding[tc.CallID] = &Call{ID: tc.CallID, Tool: tc.Name, Args: tc.Arguments, Turn: mr.TurnID, FirstAt: it.RecordedAt}
		}
	case sessionstore.ItemToolCallStatus:
		st, _ := it.Data.(sessionstore.ToolCallStatus)
		c, ok := m.Outstanding[st.CallID]
		if !ok {
			return
		}
		if st.Status.Error != "" || len(st.Status.WaitingFor) == 0 || allTerminal(st) {
			delete(m.Outstanding, st.CallID)
			m.Pending++
			m.Reasons = append(m.Reasons, "call:"+st.CallID)
			return
		}
		c.Ops = append([]operation.ID(nil), st.Status.WaitingFor...)
	}
}

// allTerminal: every op the status waits for is in it and has ended.
func allTerminal(st sessionstore.ToolCallStatus) bool {
	by := map[operation.ID]operation.Status{}
	for _, op := range st.Operations {
		by[op.ID] = op.Status
	}
	for _, id := range st.Status.WaitingFor {
		switch by[id] {
		case operation.StatusCompleted, operation.StatusFailed, operation.StatusCanceled:
		default:
			return false
		}
	}
	return true
}

// syncMirror is the replica the store observer applies, on the
// coordinator goroutine, before the coordinator's next step; the Gate
// reads it.
type syncMirror struct {
	mu sync.Mutex
	m  *Mirror
}

func (s *syncMirror) apply(it sessionstore.Item) {
	s.mu.Lock()
	s.m.Apply(it)
	s.mu.Unlock()
}

func (s *syncMirror) turnReasons() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.m.TurnReasons...)
}
