// Package stepgate is a test hook that holds bough at named points, so
// a model test can act in the gaps between the steps of a race. The
// ask timeout races (tests/model/specs/ask_timeout_vs_answer.fizz) live
// in gaps a few instructions wide: between the select choosing the
// timeout and the delete under the mutex, between the call returning
// and its end reaching serve. At full speed they close before a test
// can put anything in them.
//
// It is off unless BOUGH_TEST_STEP_GATE names a directory; every method
// is a no-op on the nil Gate. A relative directory is resolved against
// the process's working directory (serve: the child's), so one serve
// gates each session's child in that session's own folder.
//
// A hold named n is announced as <dir>/n.held, passes once <dir>/n.go
// (or <dir>/open) exists, and is acknowledged as <dir>/n.done by the
// func Hold returns. A note is a value the test reads as <dir>/k.note:
// internal state (a pending entry, hlAsk) no API shows.
package stepgate

import (
	"os"
	"path/filepath"
	"time"
)

// Env names the gate directory.
const Env = "BOUGH_TEST_STEP_GATE"

// Gate is one process's view of the gate directory.
type Gate struct{ dir string }

// Open is the gate for a process working in cwd, or nil when the hook
// is off.
func Open(cwd string) *Gate {
	dir := os.Getenv(Env)
	if dir == "" {
		return nil
	}
	if !filepath.IsAbs(dir) {
		dir = filepath.Join(cwd, dir)
	}
	os.MkdirAll(dir, 0o755)
	return &Gate{dir: dir}
}

// Here is Open for this process's working directory.
func Here() *Gate {
	if os.Getenv(Env) == "" {
		return nil
	}
	cwd, _ := os.Getwd()
	return Open(cwd)
}

func (g *Gate) path(name string) string { return filepath.Join(g.dir, name) }

func (g *Gate) exists(name string) bool {
	_, err := os.Stat(g.path(name))
	return err == nil
}

// put writes a file whole: a reader that sees it sees its content.
func (g *Gate) put(name, val string) {
	tmp := g.path(name + ".tmp")
	os.WriteFile(tmp, []byte(val), 0o644)
	os.Rename(tmp, g.path(name))
}

// Hold announces name and blocks until the test lets it through. The
// returned func acknowledges the step.
func (g *Gate) Hold(name string) func() {
	g.HoldOr(name, nil)
	return g.acker(name)
}

// HoldOr is Hold that gives up when stop closes first: false then, and
// nothing is acknowledged.
func (g *Gate) HoldOr(name string, stop <-chan struct{}) bool {
	if g == nil {
		return true
	}
	g.put(name+".held", "")
	for !g.exists(name+".go") && !g.exists("open") {
		select {
		case <-stop:
			return false
		case <-time.After(5 * time.Millisecond):
		}
	}
	return true
}

func (g *Gate) acker(name string) func() {
	if g == nil {
		return func() {}
	}
	return func() { g.put(name+".done", "") }
}

// Note records val as the current value of key.
func (g *Gate) Note(key, val string) {
	if g == nil {
		return
	}
	g.put(key+".note", val)
}

// Relay forwards in to the returned channel only once the hold name is
// let through: until then a value sent on in stays there, unread, as
// it does while a select is busy choosing another case.
func Relay[T any](g *Gate, name string, in <-chan T, stop <-chan struct{}) <-chan T {
	if g == nil {
		return in
	}
	out := make(chan T, 1)
	go func() {
		if !g.HoldOr(name, stop) {
			return
		}
		select {
		case v, ok := <-in:
			if !ok {
				close(out)
				return
			}
			out <- v
		case <-stop:
		}
	}()
	return out
}

// Watch runs fn once when <dir>/name appears, then acknowledges it as
// name.done, until stop closes. It returns at once on the nil Gate.
func (g *Gate) Watch(name string, stop <-chan struct{}, fn func()) {
	if g == nil {
		return
	}
	go func() {
		for !g.exists(name) {
			select {
			case <-stop:
				return
			case <-time.After(5 * time.Millisecond):
			}
		}
		fn()
		g.put(name+".done", "")
	}()
}
