// Subagent (workers plugin) e2e: a JS provider whose parent turn spawns
// a child that runs a bash command; the child's answer re-enters the
// parent turn as the spawn result, and the child's activity lands in the
// session JSONL as sub:* entries.
package e2e

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// spawnerProvider plays both roles, keyed on the system prompt: the
// child (the workers subagent prompt) bashes 'echo from-child' then
// answers with the tool output; the parent spawns a child and answers
// with the spawn result. fence is three backticks, via JS unicode escapes (keeps the Go
// raw string valid).
const spawnerProvider = `
var fence = "\u0060\u0060\u0060";
bough.provider("spawner", function (system, messages) {
  var last = messages[messages.length - 1].content;
  if (system.indexOf("bough subagent") >= 0) {
    if (last.indexOf("[tool output]") >= 0) return "CHILD_FINAL " + last;
    return fence + "js\nconsole.log(tools.bash('echo from-child'))\n" + fence;
  }
  if (last.indexOf("[tool output]") >= 0) return "PARENT_FINAL " + last;
  return fence + "js\nconsole.log(tools.spawn('run the echo'))\n" + fence;
});
bough.setup({ provider: { default: "spawner" } });
`

func TestHeadlessSpawnSubagent(t *testing.T) {
	t.Parallel()
	b := launchHeadless(t, launchOpts{
		cwd: map[string]string{".bough/init.js": spawnerProvider},
	})
	b.send("go")
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
	out := b.out.String()

	// Child activity surfaces as sub:* events, in order, and the child's
	// bash output travels child result -> child final -> parent result ->
	// parent final reply.
	inOrder(t, out,
		"[sub:code]",
		"[sub:result] from-child",
		"[sub:assistant] CHILD_FINAL",
		"[sub:done]",
		"[assistant] PARENT_FINAL",
		"from-child",
		"[done]",
	)

	// The session JSONL replays the subagent transcript: sub:* kinds with
	// a worker number.
	files, err := filepath.Glob(filepath.Join(b.home, ".bough", "history", "*.jsonl"))
	if err != nil || len(files) != 1 {
		t.Fatalf("want 1 session file, got %v (err %v)", files, err)
	}
	f, err := os.Open(files[0])
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	kinds := map[string]bool{}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		var e struct {
			Kind string         `json:"kind"`
			Data map[string]any `json:"data"`
		}
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("bad JSONL line %q: %v", sc.Text(), err)
		}
		kinds[e.Kind] = true
		if len(e.Kind) > 4 && e.Kind[:4] == "sub:" {
			if n, ok := e.Data["worker"].(float64); !ok || n != 1 {
				t.Fatalf("%s entry missing worker number: %v", e.Kind, e.Data)
			}
		}
	}
	for _, k := range []string{"sub:assistant", "sub:code", "sub:result", "sub:done", "input", "assistant", "done"} {
		if !kinds[k] {
			t.Fatalf("history missing kind %q (have %v)", k, kinds)
		}
	}
}
