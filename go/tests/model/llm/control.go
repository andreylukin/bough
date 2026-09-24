// Package llm drives the llm-control row (plugins/llm/control.go) from
// a test: each file queued under the control dir is the model's next
// response, so a test decides, turn by turn, whether the model answers,
// fails, streams slowly or hangs until the test lets it go.
//
// The row lives in plugins/llm because only that package may import
// the engine's harness (internal/unreal/boundary_test.go); this package
// is the test side of its file protocol and imports nothing of bough's.
package llm

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// Turn is one queued model response. Mode is "ok", "error", "slow",
// "block" or "call" (one call of Tool with Args, e.g. tools.ask). Call,
// on a release, answers with that tool call after Text, so the turn
// goes on instead of ending.
type Turn struct {
	Mode    string         `json:"mode"`
	Text    string         `json:"text,omitempty"`
	Error   string         `json:"error,omitempty"`
	DelayMS int            `json:"delay_ms,omitempty"`
	Call    *Call          `json:"call,omitempty"`
	Tool    string         `json:"tool,omitempty"`
	Args    map[string]any `json:"args,omitempty"`
}

// Call is a tool call the model makes: a native tool by name, with its
// arguments. ID defaults to one the row makes up.
type Call struct {
	ID   string         `json:"id,omitempty"`
	Name string         `json:"name"`
	Args map[string]any `json:"args,omitempty"`
}

// Dir is the control dir the row reads when its config names none.
func Dir(home string) string { return filepath.Join(home, ".bough", "llm-control") }

// Queue writes turn as <name>.json. Names are taken in lexical order,
// so "001", "002", … queue several turns. The write goes through a temp
// name the row ignores, so it never reads half a file.
func Queue(t testing.TB, dir, name string, turn Turn) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	b, err := json.Marshal(turn)
	if err != nil {
		t.Fatal(err)
	}
	tmp := filepath.Join(dir, name+".tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, filepath.Join(dir, name+".json")); err != nil {
		t.Fatal(err)
	}
}

// Release lets a "block" turn named name reply.
func Release(t testing.TB, dir, name string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, name+".release"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// ReleaseWith lets a "block" turn named name reply as turn says instead
// of with its own text: {Mode: "error"} fails the held request, so a
// session can sit in "running" before the test picks the outcome.
func ReleaseWith(t testing.TB, dir, name string, turn Turn) {
	t.Helper()
	b, err := json.Marshal(turn)
	if err != nil {
		t.Fatal(err)
	}
	// Through a temp name, as Queue does: the row must never read a
	// half-written release as an empty (plain) one.
	tmp := filepath.Join(dir, name+".release-tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, filepath.Join(dir, name+".release")); err != nil {
		t.Fatal(err)
	}
}

// HoldStart parks every process that mounts the row from now on,
// before the history row (mounted after llm) writes the session's file:
// the way a test holds a session between its spawn and its history.
func HoldStart(t testing.TB, dir string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	os.Remove(filepath.Join(dir, "start.exit"))
	// A process killed while held (serve's create timeout) never removed
	// its marker; nothing is held before the hold, so all are stale.
	stale, _ := filepath.Glob(filepath.Join(dir, "start-*.held"))
	for _, f := range stale {
		os.Remove(f)
	}
	if err := os.WriteFile(filepath.Join(dir, "start.hold"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// ReleaseStart lets held processes go on starting.
func ReleaseStart(t testing.TB, dir string) {
	t.Helper()
	if err := os.Remove(filepath.Join(dir, "start.hold")); err != nil && !os.IsNotExist(err) {
		t.Fatal(err)
	}
}

// ExitStart makes held processes exit (status 3) instead of starting.
// HoldStart clears it for the next hold.
func ExitStart(t testing.TB, dir string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "start.exit"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// Held returns the pid of a process parked by HoldStart, 0 when none
// is: each announces itself as start-<pid>.held while it waits.
func Held(dir string) int {
	files, _ := filepath.Glob(filepath.Join(dir, "start-*.held"))
	for _, f := range files {
		var pid int
		if _, err := fmt.Sscanf(filepath.Base(f), "start-%d.held", &pid); err == nil {
			return pid
		}
	}
	return 0
}

// WaitHeld waits for a process to park at the hold and returns its pid.
func WaitHeld(t testing.TB, dir string, timeout time.Duration) int {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if pid := Held(dir); pid != 0 {
			return pid
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("llm-control: no process held at start after %s", timeout)
	return 0
}

// Stream makes a held "block" turn named name send text as one streamed
// fragment and go on holding, so a test can put text on screen that
// nothing has recorded yet. It returns once the row has sent it.
func Stream(t testing.TB, dir, name, text string, timeout time.Duration) {
	t.Helper()
	tmp, dst := filepath.Join(dir, name+".stream-tmp"), filepath.Join(dir, name+".stream")
	if err := os.WriteFile(tmp, []byte(text), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(tmp, dst); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(dst); os.IsNotExist(err) {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("llm-control: turn %q did not stream %q after %s", name, text, timeout)
}

// WaitTaken waits until the row has picked up turn name (it renames
// <name>.json to <name>.taken as the request starts), so a test knows
// the model call is in flight.
func WaitTaken(t testing.TB, dir, name string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(filepath.Join(dir, name+".taken")); err == nil {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("llm-control: turn %q not taken after %s", name, timeout)
}
