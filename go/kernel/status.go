package kernel

// State is a row's live lifecycle state.
type State string

const (
	StatePending  State = "pending"  // desired and enabled, but Inject() keys unsatisfied
	StateActive   State = "active"   // mounted; effects live
	StateFailed   State = "failed"   // Apply errored; retried only when the row's spec changes
	StateDisabled State = "disabled" // disabled: true in the config
)

// RowStatus is one row's live state, as reported by Rows().
type RowStatus struct {
	ID      string
	Plugin  string
	State   State
	Missing []string // pending only: unsatisfied Inject() keys
	Err     error    // failed only: the Apply error
}

// Rows reports the desired row set with live states, in config order.
func (c *Context) Rows() []RowStatus {
	c.mu.Lock()
	desired := append([]Row(nil), c.desired...)
	active := map[string]bool{}
	for _, st := range c.rows {
		active[st.row.ID] = true
	}
	failed := map[string]failure{}
	for id, f := range c.failed {
		failed[id] = f
	}
	c.mu.Unlock()

	out := make([]RowStatus, 0, len(desired))
	for _, r := range desired {
		rs := RowStatus{ID: r.ID, Plugin: r.Plugin}
		switch {
		case r.Disabled:
			rs.State = StateDisabled
		case active[r.ID]:
			rs.State = StateActive
		case failed[r.ID].err != nil:
			rs.State = StateFailed
			rs.Err = failed[r.ID].err
		default:
			rs.State = StatePending
			if f, ok := lookup(r.Plugin); ok {
				rs.Missing = missing(c, f())
			} else {
				rs.Missing = []string{"<unknown plugin " + r.Plugin + ">"}
			}
		}
		out = append(out, rs)
	}
	return out
}

// Desired returns a copy of the last desired row set (Mount/Reconcile),
// in config order — the composed specs, including each row's Config
// (which RowStatus omits). Treat the Config maps as read-only.
func (c *Context) Desired() []Row {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]Row(nil), c.desired...)
}

// pendingDesired returns desired rows that are enabled but neither
// mounted nor failed — the rows a settle pass should try to mount.
func (c *Context) pendingDesired() []Row {
	c.mu.Lock()
	defer c.mu.Unlock()
	active := map[string]bool{}
	for _, st := range c.rows {
		active[st.row.ID] = true
	}
	var out []Row
	for _, r := range c.desired {
		if r.Disabled || active[r.ID] {
			continue
		}
		if _, isFailed := c.failed[r.ID]; isFailed {
			continue
		}
		out = append(out, r)
	}
	return out
}
