package vtreal

// tools.spawnAll with three children whose runs end differently. The
// parent's block runs on the REAL codemode + workers rows so the
// children really run; only the model is the replay tape. The three
// children share one tape, and which child takes which reply depends on
// scheduling, so the checks are by count per state and by the order of
// the results array (the "[subagent N" provenance), never by which
// task got which reply.

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// spawnallPartialFailureConfig is replayConfig with the real runtime:
// codemode, tools-basic (for the child that sleeps) and workers.
func spawnallPartialFailureConfig(tape string) string {
	cfg := replayConfig(tape)
	out := strings.Replace(cfg,
		fmt.Sprintf("- id: codemode\n  plugin: replay\n  config: {file: %q, provide: codemode}", tape),
		"- id: codemode\n  plugin: codemode\n- id: tools-basic\n  plugin: tools-basic\n- id: workers\n  plugin: workers",
		1)
	if out == cfg {
		panic("spawnallPartialFailureConfig: replayConfig's codemode row changed shape")
	}
	return out
}

const spawnallPartialFailureBlock = "const r = tools.spawnAll([\"task one\", \"task two\", \"task three\"]);\n" +
	"console.log(\"LEN=\" + r.length);\n" +
	"console.log(r.map(x => String(x).split(\"\\n\")[0]).join(\"|\"));\n"

// spawnallPartialFailureTape writes a tape: the parent's spawnAll
// reply, the given child calls (reply text, or "!err" for a failed
// model call), then the parent's closing reply.
func spawnallPartialFailureTape(t *testing.T, children ...string) string {
	t.Helper()
	var ls []string
	seq := 0
	add := func(kind string, data map[string]any) {
		seq++
		b, _ := json.Marshal(map[string]any{"seq": seq, "kind": kind, "data": data})
		ls = append(ls, string(b))
	}
	add("meta", map[string]any{"cwd": "/tmp/demo"})
	add("input", map[string]any{"text": "fan out"})
	add("assistant", map[string]any{"text": "```js\n" + spawnallPartialFailureBlock + "```"})
	for _, c := range children {
		if msg, ok := strings.CutPrefix(c, "!"); ok {
			add("error", map[string]any{"text": msg})
			add("done", map[string]any{"text": ""})
			continue
		}
		add("assistant", map[string]any{"text": c})
	}
	add("assistant", map[string]any{"text": "```stop\nPARENTDONE\n```"})
	add("done", map[string]any{"text": ""})
	p := filepath.Join(t.TempDir(), "spawnall.jsonl")
	if err := os.WriteFile(p, []byte(strings.Join(ls, "\n")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// spawnallPartialFailureSession reads this run's session history.
func spawnallPartialFailureSession(t *testing.T, a *app) []history.Entry {
	t.Helper()
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []history.Entry
	for _, p := range paths {
		es, err := history.Read(p)
		if err != nil {
			t.Fatal(err)
		}
		out = append(out, es...)
	}
	return out
}

// spawnallPartialFailureNoOrphans: every sub:start has a sub:done, and
// nothing under $HOME is a worktree.
func spawnallPartialFailureNoOrphans(t *testing.T, a *app, es []history.Entry) map[int]string {
	t.Helper()
	started := map[int]bool{}
	status := map[int]string{}
	for _, e := range es {
		w := 0
		if f, ok := e.Data["worker"].(float64); ok {
			w = int(f)
		}
		switch e.Kind {
		case "sub:start":
			started[w] = true
		case "sub:done":
			status[w], _ = e.Data["status"].(string)
		}
	}
	if len(started) != 3 {
		t.Errorf("want 3 sub:start workers, got %v", started)
	}
	for w := range started {
		if _, ok := status[w]; !ok {
			t.Errorf("orphan: worker %d has sub:start but no sub:done", w)
		}
	}
	_ = filepath.WalkDir(a.home, func(p string, d fs.DirEntry, err error) error {
		if err == nil && d.IsDir() && d.Name() == "worktrees" {
			t.Errorf("worktree dir left behind: %s", p)
		}
		return nil
	})
	return status
}

func spawnallPartialFailureCount(s, state string) int {
	n := 0
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "subagent ") && strings.Contains(l, "· "+state) {
			n++
		}
	}
	return n
}

// One child reports ok, one's model stream dies, one reports failure:
// the turn goes on with all three reports, in spawn order.
func TestSpawnAllPartialFailureContinues(t *testing.T) {
	t.Parallel()
	tape := spawnallPartialFailureTape(t,
		"```stop\nStatus: ok\nFindings: ALPHA\n```",
		"!stream ended unexpectedly",
		"```stop\nStatus: failed\nFindings: BETA\n```")
	a := startCfg(t, 120, 40, spawnallPartialFailureConfig(tape))
	a.typeText("fan out")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor("PARENTDONE")
	s := a.settled()
	a.check("after spawnAll")
	for state, want := range map[string]int{"done": 1, "error": 1, "reported failure": 1} {
		if got := spawnallPartialFailureCount(s, state); got != want {
			t.Errorf("%d card(s) in state %q, want %d:\n%s", got, state, want, s)
		}
	}
	es := spawnallPartialFailureSession(t, a)
	st := spawnallPartialFailureNoOrphans(t, a, es)
	if len(st) != 3 {
		t.Errorf("sub:done statuses: %v", st)
	}
	var res string
	for _, e := range es {
		if e.Kind == "result" {
			res, _ = e.Data["text"].(string)
		}
	}
	if !strings.Contains(res, "LEN=3") {
		t.Fatalf("parent result has no 3-long array:\n%q", res)
	}
	want := "[subagent 1 · task: task one]|[subagent 2 · task: task two]|[subagent 3 · task: task three]"
	if !strings.Contains(res, want) {
		t.Errorf("results not in spawn order; want %q in:\n%q", want, res)
	}
}

// One child done, one errored, one still sleeping when esc lands.
func TestSpawnAllPartialFailureEsc(t *testing.T) {
	t.Parallel()
	tape := spawnallPartialFailureTape(t,
		"```stop\nStatus: ok\nFindings: ALPHA\n```",
		"!stream ended unexpectedly",
		"```js\ntools.bash(\"sleep 30\")\n```")
	a := startCfg(t, 120, 40, spawnallPartialFailureConfig(tape))
	a.typeText("fan out")
	a.key(uv.KeyEnter, 0)
	a.waitUntil(func(s string) bool {
		return spawnallPartialFailureCount(s, "done") == 1 && spawnallPartialFailureCount(s, "error") == 1 &&
			spawnallPartialFailureCount(s, "running") == 1
	}, "one done, one error and one running card")
	a.key(uv.KeyEsc, 0)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never ended after esc:\n%s", a.text())
	}
	a.waitUntil(func(s string) bool { return spawnallPartialFailureCount(s, "running") == 0 },
		"no card still running after esc")
	s := a.settled()
	a.check("after esc")
	if strings.Contains(s, "PARENTDONE") {
		t.Errorf("the turn went on after esc:\n%s", s)
	}
	st := spawnallPartialFailureNoOrphans(t, a, spawnallPartialFailureSession(t, a))
	n := 0
	for _, v := range st {
		if v == "cancelled" {
			n++
		}
	}
	if n != 1 {
		t.Errorf("want 1 cancelled sub:done, got statuses %v", st)
	}
	t.Run("card_says_cancelled", func(t *testing.T) {
		if got := spawnallPartialFailureCount(s, "cancelled"); got != 1 {
			t.Errorf("%d card(s) say cancelled, want 1 (distinct from the errored one):\n%s", got, s)
		}
	})
}
