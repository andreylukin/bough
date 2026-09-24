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

// Turn is one queued model response. Mode is "ok", "error", "slow" or
// "block".
type Turn struct {
	Mode    string `json:"mode"`
	Text    string `json:"text,omitempty"`
	Error   string `json:"error,omitempty"`
	DelayMS int    `json:"delay_ms,omitempty"`
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

// Say makes a held "block" turn stream text as one assistant delta
// while it stays held: live text the session never records, which is
// what a test of the ephemeral (Seq 0) path needs. The row renames
// <name>.say-<n> to <name>.said-<n> once the delta is out; says for one
// turn go out in the order of n.
func Say(t testing.TB, dir, name string, n int, text string) {
	t.Helper()
	p := filepath.Join(dir, fmt.Sprintf("%s.say-%06d", name, n))
	if err := os.WriteFile(p+"-tmp", []byte(text), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(p+"-tmp", p); err != nil {
		t.Fatal(err)
	}
}

// Said reports whether the row has streamed say n of turn name.
func Said(dir, name string, n int) bool {
	_, err := os.Stat(filepath.Join(dir, fmt.Sprintf("%s.said-%06d", name, n)))
	return err == nil
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
