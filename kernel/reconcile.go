package kernel

import (
	"fmt"
	"os"
	"reflect"
)

// applyRow runs p.Apply with attribution: Provide/Effect/On calls made
// during Apply are recorded under this row so Reconcile can undo them.
// On Apply error the row's partial effects and provides are undone.
func (c *Context) applyRow(r Row, p Plugin) error {
	st := &rowState{row: r, injects: p.Inject()}
	c.mu.Lock()
	c.current = st
	c.mu.Unlock()
	err := p.Apply(c, r.Config)
	c.mu.Lock()
	c.current = nil
	if err == nil {
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
// still-mounted row also provides the same key (last write wins means
// the surviving value may be stale; duplicate providers are a config
// smell we don't fully handle in v0).
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
		}
	}
}

// Reconcile diffs newRows by id against the mounted set. Removed or
// changed rows (plugin name, config deep-inequality, or disabled
// toggled) unmount — effects LIFO, provided keys withdrawn — then
// added and changed rows mount via the usual fixpoint.
//
// HARD PART — withdrawing a service other mounted rows Inject: for
// basic v0 we unmount the whole dependent closure too (any mounted row
// whose Inject() intersects the withdrawn keys, transitively), then
// remount those rows after the new provider lands; Mount's fixpoint
// orders that naturally. This means e.g. swapping the llm row also
// remounts the loop row, which loses its in-memory state — accepted.
//
// A bad candidate never kills the tree: cheap validation runs before
// anything unmounts, and if the remount gets stuck we tear down and
// remount the previous rows, logging loudly and returning an error.
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
	c.mu.Unlock()
	prev := make([]Row, len(mounted)) // last good tree, for restore
	for i, st := range mounted {
		prev[i] = st.row
	}

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
	// Dependent closure: a mounted row injecting a withdrawn key must
	// unmount too, and its own provides join the withdrawn set.
	for changed := true; changed; {
		changed = false
		for _, st := range mounted {
			if drop[st.row.ID] {
				continue
			}
			for _, k := range st.injects {
				if withdrawn[k] {
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

	// Unmount dropped rows in reverse mount order.
	for i := len(mounted) - 1; i >= 0; i-- {
		if drop[mounted[i].row.ID] {
			c.disposeRow(mounted[i])
		}
	}

	// Mount everything enabled that isn't mounted now: added rows,
	// changed rows (new config), and the dropped dependents.
	stillMounted := map[string]bool{}
	c.mu.Lock()
	for _, st := range c.rows {
		stillMounted[st.row.ID] = true
	}
	c.mu.Unlock()
	var toMount []Row
	for _, r := range newRows {
		if !r.Disabled && !stillMounted[r.ID] {
			toMount = append(toMount, r)
		}
	}

	if err := c.Mount(toMount); err != nil {
		fmt.Fprintf(os.Stderr, "kernel: reconcile failed: %v; restoring previous tree\n", err)
		// Restore = tear down whatever this attempt left mounted, then
		// remount the previous rows. Surviving rows lose state too;
		// accepted for v0.
		c.mu.Lock()
		left := append([]*rowState(nil), c.rows...)
		c.mu.Unlock()
		for i := len(left) - 1; i >= 0; i-- {
			c.disposeRow(left[i])
		}
		if rerr := c.Mount(prev); rerr != nil {
			return fmt.Errorf("kernel: reconcile failed (%v) and restore failed: %w", err, rerr)
		}
		return fmt.Errorf("kernel: reconcile: %w (previous tree restored)", err)
	}
	return nil
}
