// Package kernel is the plugin kernel: services, events, effects, loader.
// Everything else in bough is a plugin mounted onto a Context.
package kernel

import (
	"fmt"
	"os"
	"sync"
)

// Context holds services, event listeners, and mounted effect disposers.
type Context struct {
	mu        sync.Mutex
	services  map[string]any
	listeners map[string]map[int]func(payload any)
	nextID    int
	effects   []func()
}

// NewContext returns an empty Context.
func NewContext() *Context {
	return &Context{
		services:  map[string]any{},
		listeners: map[string]map[int]func(any){},
	}
}

// Provide registers a service (an effect) under key. Last write wins.
func (c *Context) Provide(key string, value any) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.services[key] = value
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
	return func() {
		c.mu.Lock()
		defer c.mu.Unlock()
		delete(c.listeners[event], id)
	}
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

// Effect pushes a disposer; Unmount runs them LIFO.
func (c *Context) Effect(dispose func()) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.effects = append(c.effects, dispose)
}

// Unmount runs all registered effect disposers in LIFO order.
func (c *Context) Unmount() {
	c.mu.Lock()
	effects := c.effects
	c.effects = nil
	c.mu.Unlock()
	for i := len(effects) - 1; i >= 0; i-- {
		effects[i]()
	}
}
