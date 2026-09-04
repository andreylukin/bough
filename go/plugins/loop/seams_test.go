package loop

import (
	"context"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// recordLLM records the system prompt and every message it was sent,
// and always replies with plain text (ending the turn).
type recordLLM struct {
	system   string
	messages []Message
	reply    string
}

func (l *recordLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	l.system = system
	l.messages = append([]Message(nil), messages...)
	if l.reply == "" {
		return "ok", nil
	}
	return l.reply, nil
}

// stubHooks returns canned results per event and records fired events.
type stubHooks struct {
	results  map[string]map[string]any
	fired    []string
	payloads map[string]map[string]any
}

func (h *stubHooks) Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error) {
	h.fired = append(h.fired, event)
	if h.payloads == nil {
		h.payloads = map[string]map[string]any{}
	}
	h.payloads[event] = payload
	return h.results[event], nil
}

type stubSkills struct{ blocks []string }

func (s *stubSkills) Inject(input string) []string { return s.blocks }

// buildRunner mounts the loop plugin against stub services and returns
// the runner directly so tests can call Run synchronously.
func buildRunner(t *testing.T, llm *recordLLM, code Codemode, hooks Hooks, skills Skills) *runner {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", code)
	if hooks != nil {
		kctx.Provide("hooks", hooks)
	}
	if skills != nil {
		kctx.Provide("skills", skills)
	}
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	return r
}

func collect(kinds *[]string, texts *[]string) func(kind, text string) {
	return func(kind, text string) {
		*kinds = append(*kinds, kind)
		*texts = append(*texts, text)
	}
}

func TestHookRewritesPrompt(t *testing.T) {
	llm := &recordLLM{}
	hooks := &stubHooks{results: map[string]map[string]any{
		"user-prompt-submit": {"input": "rewritten"},
	}}
	r := buildRunner(t, llm, &stubCode{}, hooks, nil)

	var kinds, texts []string
	if err := r.Run(context.Background(), "original", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if len(llm.messages) != 1 || llm.messages[0].Content != "rewritten" {
		t.Fatalf("llm saw %v, want single message %q", llm.messages, "rewritten")
	}
}

func TestHookBlocksPrompt(t *testing.T) {
	llm := &recordLLM{}
	hooks := &stubHooks{results: map[string]map[string]any{
		"user-prompt-submit": {"block": "nope"},
	}}
	r := buildRunner(t, llm, &stubCode{}, hooks, nil)

	var kinds, texts []string
	if err := r.Run(context.Background(), "do it", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if llm.messages != nil {
		t.Fatalf("llm was called with %v, want no call", llm.messages)
	}
	// The blocked turn is an error "nope" then a "done" so drains see it end.
	if len(kinds) < 2 || kinds[len(kinds)-2] != "error" || texts[len(texts)-2] != "nope" || kinds[len(kinds)-1] != "done" {
		t.Fatalf("events %v %v, want error %q then done", kinds, texts, "nope")
	}
}

func TestHookDeniesCodeExec(t *testing.T) {
	llm := &recordLLM{reply: "Trying.\n```js\nrm()\n```"}
	code := &stubCode{}
	hooks := &stubHooks{results: map[string]map[string]any{
		"pre-code-exec": {"deny": "too spicy"},
	}}
	r := buildRunner(t, llm, code, hooks, nil)

	var kinds, texts []string
	// Every step is denied, so the budget runs out — and the turn still
	// ends with an answer, not an error.
	if err := r.Run(context.Background(), "go", collect(&kinds, &texts)); err != nil {
		t.Fatalf("a spent budget is not an error any more: %v", err)
	}
	if kinds[len(kinds)-1] != "done" || kinds[len(kinds)-2] != "assistant" {
		t.Fatalf("a spent budget must still answer, got %v", kinds[len(kinds)-3:])
	}
	if len(code.ran) != 0 {
		t.Fatalf("codemode ran %q, want nothing", code.ran)
	}
	found := false
	for i, k := range kinds {
		if k == "result" && texts[i] == "[hook denied: too spicy]" {
			found = true
		}
	}
	if !found {
		t.Fatalf("no denied result in events %v %v", kinds, texts)
	}
	// history got the denial as a result entry, so the projection
	// feeds it back to the model
	got := ""
	for _, e := range r.hist.Entries() {
		if text, _ := e.Data["text"].(string); e.Kind == "result" && strings.Contains(text, "[hook denied: too spicy]") {
			got = text
		}
	}
	if got == "" {
		t.Fatalf("denial not recorded as result entry; entries %v", r.hist.Entries())
	}
}

// buildRunnerWith mounts the loop plugin against a stub llm/codemode
// plus any extra services (history/cognition/projection/...).
func buildRunnerWith(t *testing.T, llm *recordLLM, extra map[string]any) *runner {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", &stubCode{})
	for k, v := range extra {
		kctx.Provide(k, v)
	}
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	return r
}

// stubProjection collapses all entries into one fixed user message.
type stubProjection struct{ saw []history.Entry }

func (p *stubProjection) Project(entries []history.Entry) []Message {
	p.saw = append([]history.Entry(nil), entries...)
	return []Message{{Role: "user", Content: "PROJECTED"}}
}

type stubCognition struct{ base string }

func (c *stubCognition) System(base string) string {
	c.base = base
	return "COGNITION SAYS"
}

func TestProjectionOverridesMessages(t *testing.T) {
	llm := &recordLLM{}
	proj := &stubProjection{}
	r := buildRunnerWith(t, llm, map[string]any{"projection": proj})

	var kinds, texts []string
	if err := r.Run(context.Background(), "hello", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if len(llm.messages) != 1 || llm.messages[0].Content != "PROJECTED" {
		t.Fatalf("llm saw %v, want single PROJECTED message", llm.messages)
	}
	if len(proj.saw) != 1 || proj.saw[0].Kind != "input" || proj.saw[0].Data["text"] != "hello" {
		t.Fatalf("projection saw %v, want the input entry", proj.saw)
	}
}

func TestCognitionOverridesSystem(t *testing.T) {
	llm := &recordLLM{}
	cog := &stubCognition{}
	r := buildRunnerWith(t, llm, map[string]any{"cognition": cog})

	var kinds, texts []string
	if err := r.Run(context.Background(), "hello", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	if llm.system != "COGNITION SAYS" {
		t.Fatalf("llm system = %q, want cognition override", llm.system)
	}
	if !strings.Contains(cog.base, "You are bough") {
		t.Fatalf("cognition got base %q, want built default", cog.base)
	}
}

func TestHistoryServiceUsed(t *testing.T) {
	llm := &recordLLM{}
	mem := &memHistory{}
	r := buildRunnerWith(t, llm, map[string]any{"history": mem})

	var kinds, texts []string
	if err := r.Run(context.Background(), "hello", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	got := mem.Entries()
	if len(got) != 3 || got[0].Kind != "input" || got[1].Kind != "assistant" || got[2].Kind != "done" {
		t.Fatalf("history service entries = %v, want input/assistant/done", got)
	}
	if r.hist != History(mem) {
		t.Fatal("runner did not adopt the mounted history service")
	}
}

func TestDefaultProject(t *testing.T) {
	entries := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "hi"}},
		{Kind: "assistant", Data: map[string]any{"text": "running"}},
		{Kind: "code", Data: map[string]any{"text": "1+1"}},
		{Kind: "result", Data: map[string]any{"text": "2", "code": "1+1"}},
		{Kind: "error", Data: map[string]any{"text": "boom"}},
		{Kind: "done", Data: map[string]any{"text": ""}},
	}
	got := DefaultProject(entries)
	want := []Message{
		{Role: "user", Content: "hi"},
		{Role: "assistant", Content: "running"},
		{Role: "user", Content: "[tool output]\n2"},
	}
	if len(got) != len(want) {
		t.Fatalf("DefaultProject = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("DefaultProject = %v, want %v", got, want)
		}
	}
}

// A cancelled turn projects a marker into the preceding user message
// (or its own when nothing precedes it), so the model does not resume
// the interrupted work on the next unrelated question.
func TestDefaultProjectCancelled(t *testing.T) {
	entries := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "list every file"}},
		{Kind: "cancelled", Data: map[string]any{}},
		{Kind: "done", Data: map[string]any{}},
		{Kind: "input", Data: map[string]any{"text": "what time is it"}},
	}
	got := DefaultProject(entries)
	if len(got) != 2 || got[0].Role != "user" || got[0].Content != "list every file\n\n"+cancelledNote || got[1].Content != "what time is it" {
		t.Fatalf("DefaultProject = %v", got)
	}
	got = DefaultProject([]history.Entry{
		{Kind: "input", Data: map[string]any{"text": "x"}},
		{Kind: "assistant", Data: map[string]any{"text": "ok"}},
		{Kind: "cancelled", Data: map[string]any{}},
	})
	if len(got) != 3 || got[2].Role != "user" || got[2].Content != cancelledNote {
		t.Fatalf("DefaultProject after assistant = %v", got)
	}
}

// An "undo" entry projects a note naming the reverted files (its own
// user message after an assistant reply; folded into a preceding user
// message); one that reverted nothing projects nothing.
func TestDefaultProjectUndo(t *testing.T) {
	got := DefaultProject([]history.Entry{
		{Kind: "input", Data: map[string]any{"text": "x"}},
		{Kind: "assistant", Data: map[string]any{"text": "wrote a.txt"}},
		{Kind: "done", Data: map[string]any{"files": []string{"a.txt"}}},
		{Kind: "undo", Data: map[string]any{"seq_of_turn": float64(1), "files": []any{"a.txt", "b.txt"}}},
		{Kind: "undo", Data: map[string]any{"seq_of_turn": float64(0), "files": []any{}}},
	})
	if len(got) != 3 || got[2].Role != "user" || got[2].Content != undoPrefix+"a.txt, b.txt" {
		t.Fatalf("DefaultProject = %v", got)
	}
}

func TestDefaultProjectBangCommands(t *testing.T) {
	entries := []history.Entry{
		{Kind: "command", Data: map[string]any{"text": "/help"}},
		{Kind: "system", Data: map[string]any{"text": "help text"}},
		{Kind: "command", Data: map[string]any{"text": "! ls -1"}},
		{Kind: "system", Data: map[string]any{"text": "a\nb"}},
		{Kind: "command", Data: map[string]any{"text": "!pwd"}}, // output still pending
	}
	got := DefaultProject(entries)
	want := []Message{
		{Role: "user", Content: "[shell]\n$ ls -1\na\nb"},
		{Role: "user", Content: "[shell]\n$ pwd\n"},
	}
	if len(got) != len(want) {
		t.Fatalf("DefaultProject = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("DefaultProject[%d] = %q, want %q", i, got[i], want[i])
		}
	}
}

func TestStripFakeBlocks(t *testing.T) {
	reply := "Running.\n```js\nconsole.log(1)\n```\nResult:\n```output\n1\n```\n```\nguess\n```\ndone\n```python\nprint(1)\n```"
	llm := &recordLLM{reply: reply}
	mem := &memHistory{}
	r := buildRunnerWith(t, llm, map[string]any{"history": mem})
	var kinds, texts []string
	_ = r.Run(context.Background(), "go", collect(&kinds, &texts))

	got := mem.Entries()[1]
	if got.Kind != "assistant" {
		t.Fatalf("entry[1] = %v", got)
	}
	text := got.Data["text"].(string)
	want := "Running.\n```js\nconsole.log(1)\n```\nResult:\n" + removedBlock + "\n" + removedBlock + "\ndone\n```python\nprint(1)\n```"
	if text != want {
		t.Fatalf("assistant text = %q, want %q", text, want)
	}
	if !strings.Contains(llm.system, "only the runtime returns output") {
		t.Fatalf("system prompt lacks the no-fake-output rule:\n%s", llm.system)
	}
}

type stubSysctx struct{ text string }

func (s *stubSysctx) Preamble() string { return s.text }

func TestContextPreambleRereadEachTurn(t *testing.T) {
	llm := &recordLLM{}
	sc := &stubSysctx{}
	r := buildRunnerWith(t, llm, map[string]any{"context-md": sc})
	var kinds, texts []string
	_ = r.Run(context.Background(), "one", collect(&kinds, &texts))
	if strings.Contains(llm.system, "# Context") {
		t.Fatalf("no context yet, got %q", llm.system)
	}
	sc.text = "# Context: AGENTS.md\nbe terse"
	_ = r.Run(context.Background(), "two", collect(&kinds, &texts))
	if !strings.HasPrefix(llm.system, sc.text+"\n\n") {
		t.Fatalf("mid-session context not picked up:\n%s", llm.system)
	}
}

func TestPromptSectionsAppended(t *testing.T) {
	llm := &recordLLM{}
	r := buildRunnerWith(t, llm, nil)
	r.secs.Set("b", "SECTION B")
	r.secs.Set("a", "SECTION A")
	var kinds, texts []string
	_ = r.Run(context.Background(), "hi", collect(&kinds, &texts))
	if !strings.HasSuffix(llm.system, "SECTION A\n\nSECTION B") {
		t.Fatalf("sections missing or unsorted:\n%s", llm.system)
	}
	r.secs.Set("a", "")
	_ = r.Run(context.Background(), "again", collect(&kinds, &texts))
	if strings.Contains(llm.system, "SECTION A") || !strings.HasSuffix(llm.system, "SECTION B") {
		t.Fatalf("section removal not live:\n%s", llm.system)
	}
}

// The tool catalogue is generated from the tools actually registered,
// not hand-written in the prompt: it is rebuilt every turn, so a plugin
// mounted mid-session is documented and one that went away is not.
func TestToolCatalogueFromRegistry(t *testing.T) {
	llm := &recordLLM{}
	r := buildRunnerWith(t, llm, nil)
	tools := "tools.bash(cmd) -> string: run it"
	r.cat = func() string { return tools }

	var kinds, texts []string
	_ = r.Run(context.Background(), "hi", collect(&kinds, &texts))
	for _, want := range []string{"Available in the runtime:", tools, "console.log(...)"} {
		if !strings.Contains(llm.system, want) {
			t.Fatalf("catalogue missing %q:\n%s", want, llm.system)
		}
	}

	tools = "tools.spawn(task) -> string: delegate"
	_ = r.Run(context.Background(), "again", collect(&kinds, &texts))
	if !strings.Contains(llm.system, "tools.spawn(task)") || strings.Contains(llm.system, "tools.bash(cmd) -> string: run it") {
		t.Fatalf("catalogue is not rebuilt per turn:\n%s", llm.system)
	}
}

// The benchmark's brief is opt-in: a person at a terminal must not be
// told to hunt a defect in every module.
func TestTaskGuidanceIsOptIn(t *testing.T) {
	llm := &recordLLM{}
	r := buildRunnerWith(t, llm, nil)
	var kinds, texts []string
	_ = r.Run(context.Background(), "hi", collect(&kinds, &texts))
	if strings.Contains(llm.system, "Be thorough over broad briefs") {
		t.Fatalf("bench guidance leaked into the default prompt:\n%s", llm.system)
	}

	llm2 := &recordLLM{}
	r2 := buildRunnerWith(t, llm2, nil)
	r2.guidance = TaskGuidance
	_ = r2.Run(context.Background(), "hi", collect(&kinds, &texts))
	if !strings.Contains(llm2.system, "Be thorough over broad briefs") {
		t.Fatalf("task_guidance did not append the brief:\n%s", llm2.system)
	}
}

type stubStats struct {
	files []string
	exit  int
	ran   bool
}

func (s *stubStats) Take() ([]string, int, bool) {
	f, e, r := s.files, s.exit, s.ran
	s.files, s.exit, s.ran = nil, 0, false
	return f, e, r
}

func TestDoneCarriesTurnStats(t *testing.T) {
	llm := &recordLLM{}
	mem := &memHistory{}
	st := &stubStats{files: []string{"a.go", "b.go"}, exit: 2, ran: true}
	r := buildRunnerWith(t, llm, map[string]any{"history": mem, "turn-stats": st})
	var kinds, texts []string
	_ = r.Run(context.Background(), "hi", collect(&kinds, &texts))
	es := mem.Entries()
	done := es[len(es)-1]
	if done.Kind != "done" {
		t.Fatalf("last entry = %v", done)
	}
	files, _ := done.Data["files"].([]string)
	if len(files) != 2 || files[0] != "a.go" || done.Data["exit"] != 2 {
		t.Fatalf("done data = %v, want files [a.go b.go] exit 2", done.Data)
	}
	// Next turn: nothing ran, so files is empty and exit absent.
	_ = r.Run(context.Background(), "again", collect(&kinds, &texts))
	es = mem.Entries()
	done = es[len(es)-1]
	if _, has := done.Data["exit"]; has {
		t.Fatalf("exit present with no bash run: %v", done.Data)
	}
	if files, ok := done.Data["files"].([]string); !ok || files == nil || len(files) != 0 {
		t.Fatalf("files = %v (%T), want empty non-nil slice", done.Data["files"], done.Data["files"])
	}
}

func TestSkillsInjection(t *testing.T) {
	llm := &recordLLM{}
	skills := &stubSkills{blocks: []string{"[skill: wiki]\nuse the wiki cli"}}
	r := buildRunner(t, llm, &stubCode{}, nil, skills)

	var kinds, texts []string
	if err := r.Run(context.Background(), "update the wiki", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	want := "update the wiki\n\n[skill: wiki]\nuse the wiki cli"
	if len(llm.messages) != 1 || llm.messages[0].Content != want {
		t.Fatalf("llm saw %q, want %q", llm.messages, want)
	}
}

// The stop hook receives the turn it closes: the (possibly rewritten)
// input and the model's final reply, so a memory hook has something to
// remember without re-reading history.
func TestStopHookGetsTurn(t *testing.T) {
	llm := &recordLLM{reply: "hello there"}
	hooks := &stubHooks{results: map[string]map[string]any{
		"user-prompt-submit": {"input": "rewritten"},
	}}
	r := buildRunner(t, llm, &stubCode{}, hooks, nil)

	var kinds, texts []string
	if err := r.Run(context.Background(), "original", collect(&kinds, &texts)); err != nil {
		t.Fatalf("Run: %v", err)
	}
	p := hooks.payloads["stop"]
	if p["input"] != "rewritten" || p["reply"] != "hello there" {
		t.Fatalf("stop payload %v, want input=rewritten reply=hello there", p)
	}
}

// flakyLLM replies empty n times, then "fine".
type flakyLLM struct{ empties, calls int }

func (l *flakyLLM) Complete(context.Context, string, []Message) (string, error) {
	l.calls++
	if l.calls <= l.empties {
		return "", nil
	}
	return "fine", nil
}

// One empty reply is retried silently; two in a row end the turn
// with an actionable error — never a blank assistant entry.
func TestEmptyReplyRetriedThenErrors(t *testing.T) {
	for _, tc := range []struct {
		empties   int
		wantKinds string
	}{
		{1, "assistant done"},
		{2, "error done"},
	} {
		l := &flakyLLM{empties: tc.empties}
		kctx := kernel.NewContext()
		kctx.Provide("llm", l)
		kctx.Provide("codemode", &stubCode{})
		if err := (&plugin{}).Apply(kctx, nil); err != nil {
			t.Fatal(err)
		}
		r, _ := kernel.Get[*runner](kctx, "runner")
		var kinds, texts []string
		_ = r.Run(context.Background(), "go", collect(&kinds, &texts))
		kctx.Unmount()
		got := strings.Join(kinds, " ")
		if got != tc.wantKinds {
			t.Fatalf("empties=%d: events %q, want %q (texts %q)", tc.empties, got, tc.wantKinds, texts)
		}
		for _, e := range r.hist.Entries() {
			if e.Kind == "assistant" && strings.TrimSpace(e.Data["text"].(string)) == "" {
				t.Fatalf("empties=%d: blank assistant entry recorded", tc.empties)
			}
		}
		if tc.empties == 2 && !strings.Contains(texts[0], "empty reply") {
			t.Fatalf("error should name the empty reply: %q", texts[0])
		}
	}
}

// A background job's notice reaches the model as a user-side fact,
// like tool output — it is news the model must react to, not narration.
func TestDefaultProjectJobNotice(t *testing.T) {
	got := DefaultProject([]history.Entry{
		{Kind: "input", Data: map[string]any{"text": "run the tests in the background"}},
		{Kind: "assistant", Data: map[string]any{"text": "started"}},
		{Kind: "job", Data: map[string]any{"text": "job 1 [exited 0] go test ./... (3m2s)"}},
	})
	if len(got) != 3 || got[2].Role != "user" ||
		got[2].Content != jobNotePrefix+"job 1 [exited 0] go test ./... (3m2s)" {
		t.Fatalf("DefaultProject = %v", got)
	}
}

// landJobs drains the notices seam into "job" entries; with none
// pending it records nothing at all.
func TestLandJobs(t *testing.T) {
	n := &fakeNotices{pending: []string{"job 1 finished", "job 2 finished"}}
	h := &memHistory{}
	r := &runner{hist: h, notices: n}
	var kinds []string
	emit := func(kind, text string) { kinds = append(kinds, kind) }

	r.landJobs(emit)
	if len(kinds) != 2 || kinds[0] != "job" || kinds[1] != "job" {
		t.Fatalf("emitted %v", kinds)
	}
	if len(h.Entries()) != 2 {
		t.Fatalf("recorded %d entries, want 2", len(h.Entries()))
	}
	r.landJobs(emit)
	if len(kinds) != 2 {
		t.Fatalf("a second drain invented %d more events", len(kinds)-2)
	}
	// No seam mounted at all is a clean no-op, not a panic.
	(&runner{hist: h}).landJobs(emit)
}

type fakeNotices struct {
	pending []string
	wake    chan struct{}
}

func (f *fakeNotices) Take() []string {
	p := f.pending
	f.pending = nil
	return p
}

func (f *fakeNotices) Wake() <-chan struct{} { return f.wake }
