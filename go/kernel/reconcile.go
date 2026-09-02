package kernel

import (
	"fmt"
	"os"
	"reflect"
)

// applyRow runs p.Apply with attribution: Provide/Effect/On/Get calls
// made during Apply (on this goroutine) are recorded under this row so
// the lifecycle can undo them and track dependencies. On Apply error
// the row's partial effects and provides are undone.
func (c *Context) applyRow(r Row, p Plugin) error {
	st := &rowState{
		row:      r,
		injects:  p.Inject(),
		observed: map[string]bool{},
		wants:    map[string]bool{},
	}
	c.mu.Lock()
	c.current = st
	c.applyGID = gid()
	c.mu.Unlock()
	err := p.Apply(c, r.Config)
	c.mu.Lock()
	c.current = nil
	if err == nil {
		c.seq++
		st.mountSeq = c.seq
		c.rows = append(c.rows, st)
	}
	c.mu.Unlock()
	if err != nil {
		c.disposeRow(st)
		return err
	}
	return nil
}

// disposeRow runs a row's effect disposers LIFO, removes it from the
// mounted set, and withdraws its provided keys — unless another
// still-mounted row also provides the same key (a duplicate-provider
// config; the surviving value is whatever was written last).
func (c *Context) disposeRow(st *rowState) {
	for i := len(st.effects) - 1; i >= 0; i-- {
		st.effects[i]()
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	var rows []*rowState
	for _, r := range c.rows {
		if r != st {
			rows = append(rows, r)
		}
	}
	c.rows = rows
	for _, key := range st.provides {
		other := false
		for _, r := range c.rows {
			for _, k := range r.provides {
				if k == key {
					other = true
				}
			}
		}
		if !other {
			delete(c.services, key)
			delete(c.providerOf, key)
		}
	}
}

// staleLocked reports whether a mounted row's view of its dependencies
// is out of date: a key it injected or observed was re-provided after
// its mount, or a key it wanted (Get miss during Apply) has since been
// provided. The row's own provides never make it stale. Caller holds mu.
func (c *Context) staleLocked(st *rowState) (string, bool) {
	check := func(k string) bool {
		ts, ok := c.touches[k]
		return ok && ts > st.mountSeq && c.providerOf[k] != st.row.ID
	}
	for _, k := range st.injects {
		if check(k) {
			return k, true
		}
	}
	for k := range st.observed {
		if check(k) {
			return k, true
		}
	}
	for k := range st.wants {
		if check(k) {
			return k, true
		}
	}
	return "", false
}

// settle drives the tree to a fixpoint: mount every pending desired row
// whose Inject() keys are satisfied (an Apply error moves the row to
// Failed — loudly, never retried until its spec changes), then remount
// rows made stale by new or replaced providers. Each row is remounted
// for staleness at most once per settle, and passes are capped, so a
// pathological provider chain terminates with a loud log instead of a
// loop. Pending rows left at the end are logged with their missing keys.
func (c *Context) settle() {
	remounted := map[string]bool{}
	for pass := 0; ; pass++ {
		if pass >= 16 {
			fmt.Fprintln(os.Stderr, "kernel: settle did not converge after 16 passes; giving up (tree may be stale)")
			break
		}
		progressed := false

		// Mount pending rows, dependency-gated, to their own fixpoint.
		for {
			mountedAny := false
			for _, r := range c.pendingDesired() {
				factory, ok := lookup(r.Plugin)
				if !ok {
					c.fail(r, fmt.Errorf("unknown plugin %q", r.Plugin))
					mountedAny = true
					continue
				}
				p := factory()
				if len(missing(c, p)) > 0 {
					continue
				}
				if err := c.applyRow(r, p); err != nil {
					c.fail(r, err)
				}
				mountedAny = true
			}
			if !mountedAny {
				break
			}
			progressed = true
		}

		// Remount rows whose providers changed under them.
		c.mu.Lock()
		var stale []*rowState
		keys := map[string]string{}
		for _, st := range c.rows {
			if remounted[st.row.ID] {
				continue
			}
			if k, isStale := c.staleLocked(st); isStale {
				stale = append(stale, st)
				keys[st.row.ID] = k
			}
		}
		c.mu.Unlock()
		for i := len(stale) - 1; i >= 0; i-- {
			st := stale[i]
			fmt.Fprintf(os.Stderr, "kernel: row %q reloading (service %q changed)\n",
				st.row.ID, keys[st.row.ID])
			c.disposeRow(st)
			remounted[st.row.ID] = true
			progressed = true
		}

		if !progressed {
			break
		}
	}

	for _, rs := range c.Rows() {
		if rs.State == StatePending {
			fmt.Fprintf(os.Stderr, "kernel: row %q (%s) pending: missing %v\n",
				rs.ID, rs.Plugin, rs.Missing)
		}
	}
}

// fail records a row as Failed, loudly. It stays Failed — never retried
// — until a Reconcile changes its spec (plugin, config, or disabled).
func (c *Context) fail(r Row, err error) {
	fmt.Fprintf(os.Stderr, "kernel: row %q (%s) FAILED: %v\n", r.ID, r.Plugin, err)
	c.mu.Lock()
	c.failed[r.ID] = failure{row: r, err: err}
	c.mu.Unlock()
}

// Reconcile diffs newRows against the mounted set and moves the tree to
// the new desired state. Removed, disabled, or changed rows (plugin
// name, config deep-inequality, disabled toggled) unmount — effects
// LIFO, provided keys withdrawn — and every mounted row that injected
// or observed a withdrawn key unmounts with them, transitively (it
// holds a dead reference). Then settle mounts everything mountable and
// remounts dependents of new or replaced providers.
//
// Unlike Mount, Reconcile tolerates a degraded result: a row whose
// Apply errors goes Failed (logged, retried only when its spec changes)
// and a row whose Inject() keys are unsatisfied stays Pending (logged
// with the missing keys, re-applied automatically when a provider
// lands). Other rows keep going. The returned error covers only an
// invalid candidate (validated before anything unmounts — a bad config
// never kills the tree).
func (c *Context) Reconcile(newRows []Row) error {
	// Validate the candidate before touching the tree.
	seen := map[string]bool{}
	for i, r := range newRows {
		if r.ID == "" || r.Plugin == "" {
			return fmt.Errorf("kernel: reconcile: row %d: id and plugin are required", i)
		}
		if seen[r.ID] {
			return fmt.Errorf("kernel: reconcile: duplicate row id %q", r.ID)
		}
		seen[r.ID] = true
		if !r.Disabled {
			if _, ok := lookup(r.Plugin); !ok {
				return fmt.Errorf("kernel: reconcile: row %q: unknown plugin %q", r.ID, r.Plugin)
			}
		}
	}
	newByID := map[string]Row{}
	for _, r := range newRows {
		newByID[r.ID] = r
	}

	c.mu.Lock()
	mounted := append([]*rowState(nil), c.rows...)
	// Failed rows: forget failures whose row was removed or whose spec
	// changed (the change is the retry signal); same-spec failures stay
	// Failed so a reload loop never hammers a broken Apply.
	for id, f := range c.failed {
		nr, ok := newByID[id]
		if !ok || nr.Disabled || nr.Plugin != f.row.Plugin ||
			!reflect.DeepEqual(nr.Config, f.row.Config) {
			delete(c.failed, id)
		}
	}
	c.desired = append([]Row(nil), newRows...)
	c.mu.Unlock()

	// Rows to unmount: removed, disabled, or changed.
	drop := map[string]bool{}
	withdrawn := map[string]bool{}
	for _, st := range mounted {
		nr, ok := newByID[st.row.ID]
		if !ok || nr.Disabled || nr.Plugin != st.row.Plugin ||
			!reflect.DeepEqual(nr.Config, st.row.Config) {
			drop[st.row.ID] = true
			for _, k := range st.provides {
				withdrawn[k] = true
			}
		}
	}
	// Dependent closure: a mounted row injecting or observing a
	// withdrawn key must unmount too (its reference is dead), and its
	// own provides join the withdrawn set.
	depends := func(st *rowState, key string) bool {
		for _, k := range st.injects {
			if k == key {
				return true
			}
		}
		return st.observed[key]
	}
	for changed := true; changed; {
		changed = false
		for _, st := range mounted {
			if drop[st.row.ID] {
				continue
			}
			for k := range withdrawn {
				if depends(st, k) {
					drop[st.row.ID] = true
					for _, p := range st.provides {
						withdrawn[p] = true
					}
					changed = true
					break
				}
			}
		}
	}

	// Unmount dropped rows in reverse mount order, then settle: dropped
	// dependents remount once their provider lands (or stay Pending).
	for i := len(mounted) - 1; i >= 0; i-- {
		if drop[mounted[i].row.ID] {
			c.disposeRow(mounted[i])
		}
	}
	c.settle()
	return nil
}
