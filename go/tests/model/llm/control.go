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
	"strings"
	"testing"
	"time"
)

// Turn is one queued model response. Mode is "ok", "error", "slow",
// "block" or "call" (one call of Tool with Args, e.g. tools.ask); a
// release may also be "refuse" (a provider refusal with Error as its
// message, and Calls if any). Call,
// on a release, answers with that tool call after Text, so the turn
// goes on instead of ending. Calls, on an "ok" turn or a release, are
// tool calls the response makes instead of text: the engine runs them
// (in parallel) and asks again, and the next queued turn answers that
// request.
type Turn struct {
	Mode    string         `json:"mode"`
	Text    string         `json:"text,omitempty"`
	Error   string         `json:"error,omitempty"`
	DelayMS int            `json:"delay_ms,omitempty"`
	Call    *Call          `json:"call,omitempty"`
	Tool    string         `json:"tool,omitempty"`
	Args    map[string]any `json:"args,omitempty"`
	// Bash, on a release, answers with one bash tool call running it.
	Bash  string `json:"bash,omitempty"`
	Calls []Call `json:"calls,omitempty"`
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

// Request is the user messages the request that took turn name carried,
// oldest first (<name>.request, written before the turn is marked
// taken): what reached the model, which history does not record for an
// input sent at a request boundary.
func Request(dir, name string) ([]string, error) {
	b, err := os.ReadFile(filepath.Join(dir, name+".request"))
	if err != nil {
		return nil, err
	}
	var user []string
	if err := json.Unmarshal(b, &user); err != nil {
		return nil, fmt.Errorf("llm-control: %s.request: %w", name, err)
	}
	return user, nil
}

// bootDir holds the hold_boot handshake: <id>.waiting, its content the
// session's role ("main" for a project's main thread, else "session"),
// while a fresh session is held before its history file exists, and
// <id>.release to let it go on.
func bootDir(dir string) string { return filepath.Join(dir, "boot") }

// Booting is the sessions held at boot and not yet released, id to role.
func Booting(dir string) map[string]string {
	out := map[string]string{}
	ents, _ := os.ReadDir(bootDir(dir))
	for _, e := range ents {
		id, ok := strings.CutSuffix(e.Name(), ".waiting")
		if !ok {
			continue
		}
		if _, err := os.Stat(filepath.Join(bootDir(dir), id+".release")); err == nil {
			continue
		}
		b, _ := os.ReadFile(filepath.Join(bootDir(dir), e.Name()))
		out[id] = string(b)
	}
	return out
}

// WaitBooting waits until session id is held at boot and returns its
// role.
func WaitBooting(t testing.TB, dir, id string, timeout time.Duration) string {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if role, ok := Booting(dir)[id]; ok {
			return role
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("llm-control: session %q not held at boot after %s", id, timeout)
	return ""
}

// ReleaseBoot lets a session held at boot go on to write its history.
func ReleaseBoot(t testing.TB, dir, id string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(bootDir(dir), id+".release"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// Stderr makes every process mounting the row print text as one line on
// its stderr (the first to claim it): serve relays it as a live "error"
// event, as it does any line a child writes there.
func Stderr(t testing.TB, dir, name, text string) {
	t.Helper()
	p := filepath.Join(dir, name+".stderr")
	if err := os.WriteFile(p+"-tmp", []byte(text), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(p+"-tmp", p); err != nil {
		t.Fatal(err)
	}
}

// ExitBoot makes a session held at boot exit (status 3) before it
// writes its history file.
func ExitBoot(t testing.TB, dir, id string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(bootDir(dir), id+".exit"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// BreakStdinBoot makes a session held at boot drop its stdin as it is
// released: it writes its history file and lives on, but serve's write
// of the first prompt to it fails. Write it before ReleaseBoot.
func BreakStdinBoot(t testing.TB, dir, id string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(bootDir(dir), id+".nostdin"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// ForgetBoot clears what an earlier process held at boot as session id
// left behind, so a new one started under the same id (a create that
// is retried with its id) is held and reported afresh.
func ForgetBoot(t testing.TB, dir, id string) {
	t.Helper()
	for _, ext := range []string{".waiting", ".release", ".exit", ".nostdin", ".pid"} {
		if err := os.Remove(filepath.Join(bootDir(dir), id+ext)); err != nil && !os.IsNotExist(err) {
			t.Fatal(err)
		}
	}
}

// BootPID is the pid of the process held at boot as session id, 0 when
// none was.
func BootPID(dir, id string) int {
	b, err := os.ReadFile(filepath.Join(bootDir(dir), id+".pid"))
	if err != nil {
		return 0
	}
	var pid int
	fmt.Sscanf(string(b), "%d", &pid)
	return pid
}
