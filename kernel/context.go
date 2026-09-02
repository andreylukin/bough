// Package kernel is the plugin kernel: services, events, effects, loader.
// Everything else in bough is a plugin mounted onto a Context.
package kernel

import (
	"fmt"
	"os"
	"runtime"
	"strconv"
	"strings"
	"sync"
)

// rowState is the per-row bookkeeping behind the live lifecycle:
// everything a mounted row provided, observed, or deferred, so it can
// be undone and so provider changes can find their dependents.
type rowState struct {
	row      Row
	injects  []string        // snapshot of the plugin's Inject() at mount time
	provides []string        // service keys this row's Apply provided
	observed map[string]bool // keys Get during Apply that were present (incl. optional seams)
	wants    map[string]bool // keys Get during Apply that were absent (optional seams to re-check)
	effects  []func()        // disposers registered during this row's Apply
	mountSeq int64           // sequence at mount; provides newer than this make the row stale
}

// failure records a row whose Apply errored, with the spec that failed
// so a later Reconcile retries only when the spec changes.
type failure struct {
	row Row
	err error
}

// Context holds services, event listeners, mounted effect disposers,
// and the desired row set with per-row live state.
type Context struct {
	mu        sync.Mutex
	services  map[string]any
	listeners map[string]map[int]func(payload any)
	nextID    int
	effects   []func()    // disposers registered outside any row's Apply
	rows      []*rowState // mounted (Active) rows, in mount order
	current   *rowState   // row whose Apply is running; attributes calls
	applyGID  int64       // goroutine running current's Apply (guards attribution)

	desired    []Row              // last row set given to Mount/Reconcile, config order
	failed     map[string]failure // row id -> Apply failure; retried only on spec change
	providerOf map[string]string  // service key -> row id that last provided it ("" = context-level)
	touches    map[string]int64   // service key -> sequence of its latest Provide
	seq        int64
}

// NewContext returns an empty Context.
func NewContext() *Context {
	return &Context{
		services:   map[string]any{},
		listeners:  map[string]map[int]func(any){},
		failed:     map[string]failure{},
		providerOf: map[string]string{},
		touches:    map[string]int64{},
	}
}

// gid returns the current goroutine id (parsed from runtime.Stack).
// Used only to attribute Provide/Get calls to the row whose Apply is
// running: Apply runs synchronously on the mounting goroutine, so a
// call from any other goroutine (a plugin's background work) must not
// be recorded against the row being applied.
func gid() int64 {
	var buf [64]byte
	n := runtime.Stack(buf[:], false)
	s := strings.TrimPrefix(string(buf[:n]), "goroutine ")
	if i := strings.IndexByte(s, ' '); i > 0 {
		if id, err := strconv.ParseInt(s[:i], 10, 64); err == nil {
			return id
		}
	}
	return -1
}

// Provide registers a service under key. Last write wins: providing on
// an already-provided key logs a loud warning, overwrites, and (via the
// touch sequence) makes every dependent of the key stale so the next
// settle pass reloads it. During a row's Apply the key is attributed to
// that row so unmounting withdraws it.
func (c *Context) Provide(key string, value any) {
	c.mu.Lock()
	defer c.mu.Unlock()
	prov := ""
	if c.current != nil && gid() == c.applyGID {
		prov = c.current.row.ID
		c.current.provides = append(c.current.provides, key)
	}
	if _, exists := c.services[key]; exists && c.providerOf[key] != prov {
		fmt.Fprintf(os.Stderr,
			"kernel: WARNING: service %q already provided by row %q; overwritten by row %q (last write wins, dependents reload)\n",
			key, orContext(c.providerOf[key]), orContext(prov))
	}
	c.services[key] = value
	c.providerOf[key] = prov
	c.seq++
	c.touches[key] = c.seq
}

func orContext(id string) string {
	if id == "" {
		return "<context>"
	}
	return id
}

// has reports whether a service key is present.
func (c *Context) has(key string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	_, ok := c.services[key]
	return ok
}

// Get is a typed service lookup: error if absent or wrong type.
// Calls made during a row's Apply (on the applying goroutine) are
// recorded as that row's dependencies: hits mean the row reloads if the
// key is withdrawn or re-provided; misses mean the row reloads when a
// provider for the key later lands. This is how optional seams (looked
// up with Get and tolerated when absent) participate in the lifecycle
// without being declared in Inject().
func Get[T any](c *Context, key string) (T, error) {
	var zero T
	c.mu.Lock()
	v, ok := c.services[key]
	if c.current != nil && gid() == c.applyGID {
		if ok {
			c.current.observed[key] = true
		} else {
			c.current.wants[key] = true
		}
	}
	c.mu.Unlock()
	if !ok {
		return zero, fmt.Errorf("kernel: no service %q", key)
	}
	t, ok := v.(T)
	if !ok {
		return zero, fmt.Errorf("kernel: service %q is %T, not %T", key, v, zero)
	}
	return t, nil
}

// On subscribes fn to event. Returns a disposer that unsubscribes.
func (c *Context) On(event string, fn func(payload any)) func() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.listeners[event] == nil {
		c.listeners[event] = map[int]func(any){}
	}
	id := c.nextID
	c.nextID++
	c.listeners[event][id] = fn
	off := func() {
		c.mu.Lock()
		defer c.mu.Unlock()
		delete(c.listeners[event], id)
	}
	// Subscriptions made during a row's Apply are auto-disposed when the
	// row unmounts. If the plugin also Effects the disposer it runs
	// twice, which is harmless (delete is idempotent).
	if c.current != nil {
		c.current.effects = append(c.current.effects, off)
	}
	return off
}

// Emit fires event to all listeners. Fire-and-forget; a panicking
// listener is contained (logged to stderr) and does not stop the rest.
func (c *Context) Emit(event string, payload any) {
	c.mu.Lock()
	fns := make([]func(any), 0, len(c.listeners[event]))
	for _, fn := range c.listeners[event] {
		fns = append(fns, fn)
	}
	c.mu.Unlock()
	for _, fn := range fns {
		func() {
			defer func() {
				if r := recover(); r != nil {
					fmt.Fprintf(os.Stderr, "kernel: listener panic on %q: %v\n", event, r)
				}
			}()
			fn(payload)
		}()
	}
}

// Effect pushes a disposer; Unmount runs them LIFO. During a row's
// Apply the disposer is grouped under that row for the lifecycle.
func (c *Context) Effect(dispose func()) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.current != nil {
		c.current.effects = append(c.current.effects, dispose)
		return
	}
	c.effects = append(c.effects, dispose)
}

// Unmount runs all registered effect disposers in LIFO order: mounted
// rows in reverse mount order (each row's effects LIFO), then the
// row-less effects.
func (c *Context) Unmount() {
	c.mu.Lock()
	rows := c.rows
	c.rows = nil
	effects := c.effects
	c.effects = nil
	c.mu.Unlock()
	for i := len(rows) - 1; i >= 0; i-- {
		for j := len(rows[i].effects) - 1; j >= 0; j-- {
			rows[i].effects[j]()
		}
	}
	for i := len(effects) - 1; i >= 0; i-- {
		effects[i]()
	}
}
