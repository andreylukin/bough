// Engine suite: the same real binary, on `--set loop.plugin=engine-unreal`
// (go/docs/unreal-engine.md), driven by the llm-script row, a JSON tape
// of model responses. The tape is the proof of what the model saw: a
// step's "want" must appear in the trailing user-side text of the
// request it answers, and a request that does not match answers
// "[script: …]" instead, which every test rules out. So a tape that
// plays to its last step without a "[script" line is a model that saw
// the inputs, results and placeholders in the order the design says.
package e2e

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/unreal"
)

// engineProbe runs once per test binary: while the engine-unreal row is
// still the bootstrap stub, the suite skips with the stub's own text
// instead of failing every case on a row that cannot mount.
var engineProbe struct {
	once sync.Once
	skip string
}

func needEngine(t *testing.T) {
	t.Helper()
	engineProbe.once.Do(func() {
		home, cwd, _ := sandbox(t, launchOpts{})
		out, _ := runCLI(t, home, cwd, "rows", "--config", "bough.yml",
			"--set", "llm.plugin=llm-echo", "--set", "loop.plugin=engine-unreal")
		if i := strings.Index(out, "engine-unreal: not built yet"); i >= 0 {
			line, _, _ := strings.Cut(out[i:], "\n")
			engineProbe.skip = "the engine-unreal row is the bootstrap stub: " + line
		}
	})
	if engineProbe.skip != "" {
		t.Skip(engineProbe.skip)
	}
}

// tape writes an llm-script tape from raw JSON steps.
func tape(t *testing.T, steps ...string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "tape.json")
	if err := os.WriteFile(p, []byte(`{"steps":[`+strings.Join(steps, ",\n")+`]}`), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// launchEngine is launchHeadless on the engine; script "" keeps llm-echo.
func launchEngine(t *testing.T, script string, o launchOpts) *bough {
	t.Helper()
	needEngine(t)
	sets := []string{"loop.plugin=engine-unreal"}
	if script != "" {
		sets = append(sets, "llm.plugin=llm-script", "llm.script="+script)
	}
	o.sets = append(sets, o.sets...)
	return launchHeadless(t, o)
}

// finish closes stdin and requires a clean exit and a tape that never
// answered out of order.
func (b *bough) finish() string {
	b.t.Helper()
	b.closeStdin()
	if code := b.waitExit(); code != 0 {
		b.t.Fatalf("exit %d; output:\n%s", code, b.out.String())
	}
	out := b.out.String()
	mustNotContain(b.t, out, "[script")
	return out
}

// waitCount polls the output until substr appears n times.
func (b *bough) waitCount(substr string, n int) {
	b.t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Count(b.out.String(), substr) >= n {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	b.t.Fatalf("%q not in output %d times after 30s; output:\n%s", substr, n, b.out.String())
}

type entry struct {
	Seq  int64          `json:"seq"`
	At   time.Time      `json:"at"`
	Kind string         `json:"kind"`
	Data map[string]any `json:"data"`
}

func readEntries(t *testing.T, path string) []entry {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	var es []entry
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 8*1024*1024)
	for sc.Scan() {
		var e entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("bad JSONL line %q: %v", sc.Text(), err)
		}
		es = append(es, e)
	}
	return es
}

// onlySession is the one history file under home.
func onlySession(t *testing.T, home string) string {
	t.Helper()
	files := sessionFiles(t, home)
	if len(files) != 1 {
		t.Fatalf("want 1 session file, got %v", files)
	}
	return files[0]
}

func kinds(es []entry, kind string) []entry {
	var out []entry
	for _, e := range es {
		if e.Kind == kind {
			out = append(out, e)
		}
	}
	return out
}

func dump(es []entry) string {
	var b strings.Builder
	for _, e := range es {
		d, _ := json.Marshal(e.Data)
		fmt.Fprintf(&b, "%d %s %s\n", e.Seq, e.Kind, d)
	}
	return b.String()
}

func num(v any) (float64, bool) {
	f, ok := v.(float64)
	return f, ok
}

// lastIn is a done entry's usage.last_in: llm-script bills 100 input
// tokens per request item, so it measures how much context the model
// was sent in that turn's last request.
func lastIn(t *testing.T, e entry) float64 {
	t.Helper()
	u, _ := e.Data["usage"].(map[string]any)
	n, ok := num(u["last_in"])
	if !ok || n <= 0 {
		t.Fatalf("done has no usage.last_in: %v", e.Data)
	}
	return n
}

// events parses --json output, skipping the launcher's plain stderr.
func events(out string) []map[string]any {
	var evs []map[string]any
	for line := range strings.SplitSeq(out, "\n") {
		var ev map[string]any
		if strings.HasPrefix(line, "{") && json.Unmarshal([]byte(line), &ev) == nil {
			evs = append(evs, ev)
		}
	}
	return evs
}

// The AGENTS.md smoke, on the engine: echo's CODE! is a native bash
// call, and its result is the newest input of the next request.
func TestEngineEchoSmoke(t *testing.T) {
	t.Parallel()
	b := launchEngine(t, "", launchOpts{})
	b.send("say CODE! please")
	out := b.finish()
	inOrder(t, out, "[call]", "ran: hi from codemode", "[done]")
}

func TestEngineText(t *testing.T) {
	t.Parallel()
	b := launchEngine(t, tape(t, `{"want": "hello engine", "text": "hi from the tape"}`), launchOpts{})
	b.send("hello engine")
	out := b.finish()
	inOrder(t, out, "[assistant] hi from the tape", "[done]")
	if n := strings.Count(out, "[done]"); n != 1 {
		t.Fatalf("[done] printed %d times, want 1:\n%s", n, out)
	}
}

// One native call, read two ways: the --json event stream (a live call
// start, then the recorded end with its exit) and the history file
// (every §10.1 key the readers depend on).
func TestEngineToolCallJSONAndHistory(t *testing.T) {
	t.Parallel()
	script := tape(t,
		`{"want": "run it", "calls": [{"id": "c1", "name": "bash", "args": {"command": "echo TOOL_OUT_41"}}]}`,
		`{"want": "TOOL_OUT_41", "text": "saw TOOL_OUT_41"}`,
	)
	b := launchEngine(t, script, launchOpts{args: []string{"--json"}})
	b.send("run it")
	out := b.finish()

	evs := events(out)
	start, end, reply, done := -1, -1, -1, 0
	for i, ev := range evs {
		switch ev["kind"] {
		case "call":
			if ev["id"] != "c1" {
				continue
			}
			if ev["phase"] == "start" {
				if start < 0 {
					start = i
				}
				if ev["tool"] != "bash" || ev["text"] != "echo TOOL_OUT_41" {
					t.Fatalf("call start = %v, want tool bash, text the command", ev)
				}
			} else if exit, ok := num(ev["exit"]); ok && exit == 0 {
				end = i
			}
		case "assistant":
			if ev["text"] == "saw TOOL_OUT_41" {
				reply = i
			}
		case "done":
			done++
		}
		if ev["kind"] == "error" {
			t.Fatalf("error event %v:\n%s", ev, out)
		}
	}
	if start < 0 || end < 0 || reply < 0 || !(start < end && end < reply) {
		t.Fatalf("want call start < call end (exit 0) < reply; got %d, %d, %d:\n%s", start, end, reply, out)
	}
	if done != 1 {
		t.Fatalf("%d done events, want 1:\n%s", done, out)
	}

	es := readEntries(t, onlySession(t, b.home))
	for i := range es {
		if es[i].Seq != int64(i+1) || es[i].At.IsZero() {
			t.Fatalf("entry %d: seq %d at %v; want seq %d and a timestamp:\n%s", i, es[i].Seq, es[i].At, i+1, dump(es))
		}
	}
	if len(kinds(es, "code")) > 0 || len(kinds(es, "result")) > 0 {
		t.Fatalf("native calls write call rows, never code/result:\n%s", dump(es))
	}

	eng := kinds(es, "engine")
	if len(eng) == 0 {
		t.Fatalf("no engine entry:\n%s", dump(es))
	}
	ed := eng[0].Data
	sha, _ := ed["sha"].(string)
	sid, _ := ed["session"].(string)
	if ed["engine"] != "unreal" || ed["pin"] != unreal.Pin || len(sha) < 7 || !strings.HasPrefix(unreal.PinSHA, sha) || sid == "" {
		t.Fatalf("engine entry %v: want engine unreal, pin %s, sha a prefix of %s, a session", ed, unreal.Pin, unreal.PinSHA)
	}
	if f, _ := num(ed["format"]); f != 2 {
		t.Fatalf("engine entry format %v, want 2 (localfile v2 at the pin)", ed["format"])
	}
	for _, k := range []string{"system", "tools"} {
		if s, _ := ed[k].(string); s == "" {
			t.Fatalf("engine entry has no %s hash: %v", k, ed)
		}
	}
	for _, p := range []string{sid + ".session.jsonl", sid + ".system.md"} {
		if _, err := os.Stat(filepath.Join(b.home, ".bough", "engine", p)); err != nil {
			t.Fatalf("engine file %s: %v", p, err)
		}
	}

	in, calls, asst, dn := kinds(es, "input"), kinds(es, "call"), kinds(es, "assistant"), kinds(es, "done")
	if len(in) != 1 || len(calls) != 1 || len(asst) != 1 || len(dn) != 1 {
		t.Fatalf("want one input, call, assistant and done:\n%s", dump(es))
	}
	if id, _ := in[0].Data["input_id"].(string); id == "" || in[0].Data["text"] != "run it" {
		t.Fatalf("input entry %v: want the text and an input_id", in[0].Data)
	}
	c := calls[0].Data
	if c["tool"] != "bash" || c["id"] != "c1" || c["text"] != "echo TOOL_OUT_41" {
		t.Fatalf("call entry %v: want tool bash, id c1, the command as text", c)
	}
	if exit, ok := num(c["exit"]); !ok || exit != 0 {
		t.Fatalf("call entry exit %v, want 0", c["exit"])
	}
	if o, _ := c["output"].(string); !strings.Contains(o, "TOOL_OUT_41") {
		t.Fatalf("call entry output %q does not hold the command's output", c["output"])
	}
	if _, ok := num(c["ms"]); !ok {
		t.Fatalf("call entry has no ms: %v", c)
	}
	callSeq, ok1 := num(c["hseq"])
	asstSeq, ok2 := num(asst[0].Data["hseq"])
	if !ok1 || !ok2 || callSeq <= 0 || asstSeq <= callSeq {
		t.Fatalf("hseq: call %v, assistant %v; want both, increasing (catch-up keys on it)", c["hseq"], asst[0].Data["hseq"])
	}
	if asst[0].Data["text"] != "saw TOOL_OUT_41" {
		t.Fatalf("assistant entry %v", asst[0].Data)
	}
	if _, ok := asst[0].Data["provider"]; !ok {
		t.Fatalf("assistant entry names no provider: %v", asst[0].Data)
	}
	if turn, _ := dn[0].Data["engine_turn"].(string); turn == "" {
		t.Fatalf("done entry has no engine_turn (fork reads it): %v", dn[0].Data)
	}
	lastIn(t, dn[0])
	if !(in[0].Seq < calls[0].Seq && calls[0].Seq < asst[0].Seq && asst[0].Seq < dn[0].Seq) {
		t.Fatalf("want input < call < assistant < done:\n%s", dump(es))
	}
}

// Three calls in one response run at once: each waits (up to 5 s) for
// the other two to have started, which a serial runner never lets
// happen. The model then reads all three results.
func TestEngineParallelFanOut(t *testing.T) {
	t.Parallel()
	call := func(me, a, b string) string {
		cmd := fmt.Sprintf(`touch %[1]s; for i in $(seq 50); do [ -e %[2]s ] && [ -e %[3]s ] && break; sleep 0.1; done; [ -e %[2]s ] && [ -e %[3]s ] && echo FAN_%[1]s saw=all`, me, a, b)
		args, _ := json.Marshal(map[string]string{"command": cmd})
		return fmt.Sprintf(`{"id": "fan_%s", "name": "bash", "args": %s}`, me, args)
	}
	script := tape(t,
		`{"want": "fan out", "calls": [`+call("A", "B", "C")+`, `+call("B", "A", "C")+`, `+call("C", "A", "B")+`]}`,
		`{"want": "saw=all", "text": "all three answered"}`,
		// The harness batches results that land within 1 s of each
		// other; on a starved machine one may miss that window and
		// arrive in a request of its own, which this step answers.
		`{"want": "saw=all", "text": "and the straggler"}`,
	)
	b := launchEngine(t, script, launchOpts{})
	b.send("fan out")
	out := b.finish()
	mustContain(t, out, "[assistant] all three answered")
	if n := strings.Count(out, "[done]"); n != 1 {
		t.Fatalf("[done] printed %d times, want 1:\n%s", n, out)
	}
	es := readEntries(t, onlySession(t, b.home))
	seen := map[string]bool{}
	for _, c := range kinds(es, "call") {
		id, _ := c.Data["id"].(string)
		o, _ := c.Data["output"].(string)
		if !strings.Contains(o, "saw=all") {
			t.Fatalf("call %s never saw the other two running: %q\n%s", id, o, dump(es))
		}
		seen[id] = true
	}
	if len(seen) != 3 {
		t.Fatalf("want 3 call rows, got %v:\n%s", seen, dump(es))
	}
}

// A call that outlives the 1 s grace: the model first reads its
// placeholder beside the fast call's result and ends its reply, then
// reads the late result and answers, all inside one bough turn.
func TestEngineInProgressThenFinal(t *testing.T) {
	t.Parallel()
	script := tape(t,
		`{"want": "slow please", "calls": [
			{"id": "fast", "name": "bash", "args": {"command": "echo FAST_1"}},
			{"id": "slow", "name": "bash", "args": {"command": "sleep 3; echo LATE_2"}}]}`,
		`{"want": "still running", "text": "FAST_1 is in; waiting on the slow one"}`,
		`{"want": "LATE_2", "text": "final: LATE_2 arrived"}`,
	)
	b := launchEngine(t, script, launchOpts{})
	b.send("slow please")
	out := b.finish()
	inOrder(t, out, "[assistant] FAST_1 is in; waiting on the slow one", "[assistant] final: LATE_2 arrived", "[done]")
	if n := strings.Count(out, "[done]"); n != 1 {
		t.Fatalf("[done] printed %d times, want 1 (a foreground call holds its turn):\n%s", n, out)
	}
	es := readEntries(t, onlySession(t, b.home))
	for _, c := range kinds(es, "call") {
		if c.Data["id"] == "slow" {
			if c.Data["late"] != true {
				t.Fatalf("the slow call's row is not marked late: %v", c.Data)
			}
			return
		}
	}
	t.Fatalf("no call row for the slow call:\n%s", dump(es))
}

// ask is a blocking call: the headless line after "[ask]" answers it,
// and the model reads the answer in the same turn.
func TestEngineAsk(t *testing.T) {
	t.Parallel()
	script := tape(t,
		`{"want": "pick one", "calls": [{"id": "q1", "name": "ask", "args": {"question": "fav color?", "options": ["red", "blue"]}}]}`,
		`{"want": "blue", "text": "final: blue it is"}`,
	)
	b := launchEngine(t, script, launchOpts{})
	b.send("pick one")
	b.waitFor("[ask] fav color?")
	b.waitFor("2. blue")
	b.send("2")
	b.waitFor("final: blue it is")
	out := b.finish()
	if n := strings.Count(out, "[done]"); n != 1 {
		t.Fatalf("[done] printed %d times, want 1:\n%s", n, out)
	}
	es := readEntries(t, onlySession(t, b.home))
	if len(kinds(es, "ask")) != 1 || len(kinds(es, "ask/answer")) != 1 {
		t.Fatalf("want one ask and one ask/answer entry:\n%s", dump(es))
	}
}

// A line sent while a model request is in flight steers the turn: it
// lands after that response (never cutting paid output), the model is
// asked again, and the turn still ends in exactly one done.
func TestEngineSteer(t *testing.T) {
	t.Parallel()
	script := tape(t,
		`{"want": "first", "hold_ms": 4000, "text": "reply one"}`,
		`{"want": "second", "text": "reply two"}`,
	)
	b := launchEngine(t, script, launchOpts{})
	b.send("first")
	time.Sleep(700 * time.Millisecond) // inside the held request
	b.send("second")
	out := b.finish()
	inOrder(t, out, "[steer] second", "[assistant] reply one", "[assistant] reply two", "[done]")
	if n := strings.Count(out, "[done]"); n != 1 {
		t.Fatalf("[done] printed %d times, want 1:\n%s", n, out)
	}
	es := readEntries(t, onlySession(t, b.home))
	steers := 0
	for _, e := range kinds(es, "input") {
		if e.Data["steer"] == true && e.Data["text"] == "second" {
			steers++
		}
	}
	if steers != 1 {
		t.Fatalf("want one input{steer} entry for the steer:\n%s", dump(es))
	}
}

// SIGINT during a call: the call is cancelled, the turn records
// cancelled then done, the process exits 130, and the next message on
// -r reaches the model with no request in between.
func TestEngineCancelThenResume(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("no SIGINT to a child on Windows")
	}
	a := launchEngine(t, tape(t,
		`{"want": "go long", "calls": [{"id": "long", "name": "bash", "args": {"command": "sleep 30; echo NEVER_PRINTED"}}]}`,
	), launchOpts{args: []string{"--json"}})
	a.send("go long")
	a.waitFor(`"phase":"start"`)
	if err := a.cmd.Process.Signal(os.Interrupt); err != nil {
		t.Fatal(err)
	}
	if code := a.waitExit(); code != 130 {
		t.Fatalf("exit %d, want 130; output:\n%s", code, a.out.String())
	}
	mustNotContain(t, a.out.String(), "NEVER_PRINTED", "[script")
	path := onlySession(t, a.home)
	es := readEntries(t, path)
	for i, e := range es {
		if e.Kind == "cancelled" && (i+1 >= len(es) || es[i+1].Kind != "done") {
			t.Fatalf("cancelled is not immediately followed by done:\n%s", dump(es))
		}
	}
	if len(kinds(es, "cancelled")) != 1 || len(kinds(es, "done")) != 1 {
		t.Fatalf("want one cancelled and one done:\n%s", dump(es))
	}
	canceled := false
	for _, c := range kinds(es, "call") {
		canceled = canceled || (c.Data["id"] == "long" && c.Data["canceled"] == true)
	}
	if !canceled {
		t.Fatalf("no canceled call row for the interrupted call:\n%s", dump(es))
	}

	id := strings.TrimSuffix(filepath.Base(path), ".jsonl")
	b := launchEngine(t, tape(t, `{"want": "next message", "text": "back after the cancel"}`),
		launchOpts{from: a, args: []string{"-r", id}})
	b.send("next message")
	out := b.finish()
	inOrder(t, out, "[assistant] back after the cancel", "[done]")
	if after := sessionFiles(t, a.home); len(after) != 1 {
		t.Fatalf("resume must not create a session file: %v", after)
	}
}

// -r resumes the harness session behind the history file: the model is
// sent the earlier turn (more context than a fresh session's first
// request), and the same history file grows.
func TestEngineResume(t *testing.T) {
	t.Parallel()
	a := launchEngine(t, tape(t, `{"want": "first", "text": "one"}`), launchOpts{})
	a.send("first")
	mustContain(t, a.finish(), "[assistant] one")
	path := onlySession(t, a.home)
	before := readEntries(t, path)
	firstIn := lastIn(t, kinds(before, "done")[0])

	id := strings.TrimSuffix(filepath.Base(path), ".jsonl")
	b := launchEngine(t, tape(t, `{"want": "second", "text": "two"}`), launchOpts{from: a, args: []string{"-r", id}})
	b.send("second")
	mustContain(t, b.finish(), "[assistant] two")
	if after := sessionFiles(t, a.home); len(after) != 1 || after[0] != path {
		t.Fatalf("resume must reuse %s, got %v", path, after)
	}
	es := readEntries(t, path)
	dn := kinds(es, "done")
	if len(dn) != 2 {
		t.Fatalf("want 2 done entries after the resumed turn:\n%s", dump(es))
	}
	if secondIn := lastIn(t, dn[1]); secondIn <= firstIn {
		t.Fatalf("resumed request carried %v input tokens, the first %v: the earlier turn did not reach the model", secondIn, firstIn)
	}
}

// /tree forks at the first of two turns: the fork's model sees that
// turn and not the second, through the harness fork (not a reseed), and
// the fork records where it came from.
func TestEngineFork(t *testing.T) {
	t.Parallel()
	a := launchEngine(t, tape(t,
		`{"want": "first", "text": "one"}`,
		`{"want": "second", "text": "SECOND_TURN_REPLY"}`,
	), launchOpts{})
	a.send("first")
	a.waitFor("[assistant] one")
	a.send("second")
	// /tree refuses a turn with no done yet.
	a.waitCount("[done]", 2)
	path := onlySession(t, a.home)
	parent := readEntries(t, path)
	ins := kinds(parent, "input")
	if len(ins) != 2 {
		t.Fatalf("want 2 input entries:\n%s", dump(parent))
	}
	a.send(fmt.Sprintf("/tree %d", ins[0].Seq))
	a.waitFor("(bough --resume ")
	out := a.finish()
	m := regexp.MustCompile(`session (\S+) \(bough --resume`).FindStringSubmatch(out)
	if m == nil {
		t.Fatalf("no fork id in:\n%s", out)
	}
	parentTurn2 := lastIn(t, kinds(readEntries(t, path), "done")[1])

	b := launchEngine(t, tape(t, `{"want": "third", "text": "three"}`), launchOpts{from: a, args: []string{"-r", m[1]}})
	b.send("third")
	mustContain(t, b.finish(), "[assistant] three")

	fork := readEntries(t, filepath.Join(filepath.Dir(path), m[1]+".jsonl"))
	for _, e := range kinds(fork, "assistant") {
		if e.Data["text"] == "SECOND_TURN_REPLY" {
			t.Fatalf("the fork carries the turn after the fork point:\n%s", dump(fork))
		}
	}
	forked := false
	for _, e := range kinds(fork, "engine") {
		if s, _ := e.Data["forked_from"].(string); s != "" {
			forked = true
		}
		if e.Data["seeded"] != nil {
			t.Fatalf("the fork was reseeded instead of forked in the store: %v", e.Data)
		}
	}
	if !forked {
		t.Fatalf("no engine{forked_from} entry in the fork:\n%s", dump(fork))
	}
	// [system, first, one, third] is as long as the parent's second
	// request [system, first, one, second]: the fork's context is the
	// fork point's, neither the whole parent nor a one-message reseed.
	dn := kinds(fork, "done")
	if got := lastIn(t, dn[len(dn)-1]); got != parentTurn2 {
		t.Fatalf("the fork's request carried %v input tokens, the parent's second %v", got, parentTurn2)
	}
}

// spawn {background: true} goes to serve's API as today: a stub serve
// answers the create, and the model reads the new session's id.
func TestEngineBackgroundAgent(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var got []map[string]any
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost || r.URL.Path != "/api/sessions" {
			http.NotFound(w, r)
			return
		}
		body, _ := io.ReadAll(r.Body)
		var req map[string]any
		_ = json.Unmarshal(body, &req)
		mu.Lock()
		got = append(got, req)
		mu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"session": {"id": "child-7"}, "queued": false}`)
	}))
	defer srv.Close()

	// serveclient finds serve through this record; the pid must be alive,
	// so it is this test process.
	pid := fmt.Sprintf("%d %s\n", os.Getpid(), strings.TrimPrefix(srv.URL, "http://"))
	b := launchEngine(t, tape(t,
		`{"want": "delegate", "calls": [{"id": "bg", "name": "spawn", "args": {"task": "write the report", "background": true}}]}`,
		`{"want": "child-7", "text": "started child-7"}`,
	), launchOpts{home: map[string]string{".bough/serve.pid": pid}})
	b.send("delegate")
	out := b.finish()
	inOrder(t, out, "[assistant] started child-7", "[done]")

	mu.Lock()
	defer mu.Unlock()
	if len(got) != 1 {
		t.Fatalf("serve saw %d creates, want 1", len(got))
	}
	id := strings.TrimSuffix(filepath.Base(onlySession(t, b.home)), ".jsonl")
	if got[0]["prompt"] != "write the report" || got[0]["spawnedBy"] != id {
		t.Fatalf("create request %v: want the task as prompt and spawnedBy %s", got[0], id)
	}
}
