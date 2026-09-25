// Package linegate is a test hook that lets a model test step the two
// pipes between `bough serve` and a session's child one line at a time:
// serve's stdout pump (which arms a question) and the child's stdin
// reader (which routes a prompt or an answer). Answer delivery races in
// the gaps between them (tests/model/specs/ask_answer_arm_races.fizz),
// and at full speed those gaps close before a test can put anything in
// them.
//
// It is off unless BOUGH_TEST_LINE_GATE names a directory. A relative
// one is resolved against the child's working directory, so one serve
// gates each session's child in that session's own folder. Line n on a
// side is announced as <dir>/<side>.<n>.held (holding its description),
// handled once <dir>/<side>.<n>.go exists, and acknowledged as
// <side>.<n>.done (holding the outcome). <dir>/open lets every line
// through, so a test can let a child it is done with drain and exit.
package linegate

import (
	"os"
	"path/filepath"
	"strconv"
	"time"
)

// Env names the gate directory.
const Env = "BOUGH_TEST_LINE_GATE"

// Gate numbers the lines of one side of one child. A nil Gate lets
// every line through at once.
type Gate struct {
	dir, side string
	n         int
}

// Open is the gate for side ("out" or "in") of the child working in
// cwd, or nil when the hook is off.
func Open(side, cwd string) *Gate {
	dir := os.Getenv(Env)
	if dir == "" {
		return nil
	}
	if !filepath.IsAbs(dir) {
		dir = filepath.Join(cwd, dir)
	}
	os.MkdirAll(dir, 0o755)
	return &Gate{dir: dir, side: side}
}

// Hold announces the next line and blocks until the test lets it
// through. The returned func acknowledges it with its outcome.
func (g *Gate) Hold(desc string) func(outcome string) {
	if g == nil {
		return func(string) {}
	}
	base := filepath.Join(g.dir, g.side+"."+strconv.Itoa(g.n))
	g.n++
	os.WriteFile(base+".held", []byte(desc), 0o644)
	for {
		if _, err := os.Stat(base + ".go"); err == nil {
			break
		}
		if _, err := os.Stat(filepath.Join(g.dir, "open")); err == nil {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	return func(outcome string) {
		// Renamed into place: a reader that sees .done sees its outcome.
		os.WriteFile(base+".tmp", []byte(outcome), 0o644)
		os.Rename(base+".tmp", base+".done")
	}
}
