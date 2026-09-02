// Package kernel is the plugin kernel: services, events, effects, loader.
// Everything else in bough is a plugin mounted onto a Context.
package kernel

import (
	"fmt"
	"os"
	"sync"
)

// rowState is the per-row bookkeeping behind Reconcile: everything a
// mounted row provided, subscribed, or deferred, so it can be undone.
type rowState struct {
	row      Row
	injects  []string // snapshot of the plugin's Inject() at mount time
	provides []string // service keys this row's Apply provided
	effects  []func() // disposers registered during this row's Apply
}

// Context holds services, event listeners, and mounted effect disposers.
type Context struct {
	mu        sync.Mutex
	services  map[string]any
	listeners map[string]map[int]func(payload any)
	nextID    int
	effects   []func()    // disposers registered outside any row's Apply
	rows      []*rowState // mounted rows, in mount order
	current   *rowState   // row whose Apply is running; attributes calls
}

// NewContext returns an empty Context.
func NewContext() *Context {
	return &Context{
		services:  map[string]any{},
		listeners: map[string]map[int]func(any){},
	}
}

// Provide registers a service (an effect) under key. Last write wins.
// During a row's Apply the key is attributed to that row so Reconcile
// can withdraw it on unmount.
func (c *Context) Provide(key string, value any) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.services[key] = value
	if c.current != nil {
		c.current.provides = append(c.current.provides, key)
	}
}

// has reports whether a service key is present.
func (c *Context) has(key string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	_, ok := c.services[key]
	return ok
}

// Get is a typed service lookup: error if absent or wrong type.
func Get[T any](c *Context, key string) (T, error) {
	var zero T
	c.mu.Lock()
	v, ok := c.services[key]
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
// Apply the disposer is grouped under that row for Reconcile.
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
