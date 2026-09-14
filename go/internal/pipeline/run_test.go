package pipeline

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"
)

func newRun(t *testing.T, p *Pipeline, f *fakeSessions) *Runner {
	t.Helper()
	r, err := NewRunner(p, Options{Home: t.TempDir(), Sessions: f, Sets: []string{"llm.plugin=llm-echo"}})
	if err != nil {
		t.Fatal(err)
	}
	return r
}

func echo(id, prompt string) string { return "ok: " + prompt }

func TestRunCheckPassAndRunDir(t *testing.T) {
	p, err := writePipeline(t, validYML, nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(echo)
	r := newRun(t, p, f)
	st, err := r.Run(context.Background())
	if err != nil || st.Status != StatusPassed {
		t.Fatalf("status = %s, %v", st.Status, err)
	}
	for _, name := range []string{"pipeline.yml", "state.json", "events.jsonl",
		"steps/1-coder/prompt.md", "steps/1-coder/reply.md", "steps/1-coder/routed.md", "steps/1-coder/result.json",
		"steps/2-tests/output.txt", "steps/2-tests/result.json"} {
		if _, err := os.Stat(filepath.Join(r.Dir(), name)); err != nil {
			t.Errorf("run dir: %v", err)
		}
	}
	prompt, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/1-coder/prompt.md"))
	if string(prompt) != "coder 1: make it green / " {
		t.Errorf("prompt.md = %q", prompt)
	}
	saved, err := ReadState(r.opt.Home, r.ID())
	if err != nil {
		t.Fatal(err)
	}
	if saved.Status != st.Status || saved.Step != st.Step || saved.Visits["tests"] != 1 || saved.Sessions["coder"] != st.Sessions["coder"] || saved.Ended.IsZero() {
		t.Errorf("state.json = %+v, returned %+v", saved, st)
	}
	evs, _ := ReadEvents(r.opt.Home, r.ID())
	var kinds []string
	for _, e := range evs {
		kinds = append(kinds, e.Kind)
	}
	want := []string{"start", "visit", "prompt", "reply", "route", "visit", "check", "route", "end"}
	if !slices.Equal(kinds, want) {
		t.Errorf("events = %v, want %v", kinds, want)
	}
	if c := f.created[0]; c.Origin != "loop" || c.ID == "" || !slices.Contains(c.Args, "llm.plugin=llm-echo") {
		t.Errorf("create = %+v", c)
	}
	if runs, _ := ListRuns(r.opt.Home); len(runs) != 1 || runs[0].ID != r.ID() {
		t.Errorf("ListRuns = %+v", runs)
	}
}

func TestRunCheckFailRoutesBackThenExhausts(t *testing.T) {
	yml := strings.Replace(validYML, `run: "exit 0"`, `run: "echo broken; exit 1"`, 1)
	p, err := writePipeline(t, yml, nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(echo)
	r := newRun(t, p, f)
	st, _ := r.Run(context.Background())
	if st.Status != StatusExhausted {
		t.Fatalf("status = %s", st.Status)
	}
	// The coder's second visit sees the check's output; fresh sessions each time.
	prompt, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/3-coder/prompt.md"))
	if !strings.Contains(string(prompt), "coder 2") || !strings.Contains(string(prompt), "broken") {
		t.Errorf("prompt 2 = %q", prompt)
	}
	if f.createdCount() != 3 {
		t.Errorf("fresh sessions created = %d, want 3", f.createdCount())
	}
	res, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/2-tests/result.json"))
	if !strings.Contains(string(res), `"exit": 1`) || !strings.Contains(string(res), `"route": "coder"`) {
		t.Errorf("result.json = %s", res)
	}
}

func TestRunCheckFailToFail(t *testing.T) {
	yml := strings.Replace(strings.Replace(validYML, `run: "exit 0"`, `run: "exit 1"`, 1), "    fail: coder\n    max_visits: 3\n", "    fail: fail\n    max_visits: 3\n", 1)
	yml = strings.Replace(yml, "    pass: done\n    fail: coder", "    pass: done\n    fail: fail", 1)
	p, err := writePipeline(t, yml, nil)
	if err != nil {
		t.Fatal(err)
	}
	st, _ := newRun(t, p, newFake(echo)).Run(context.Background())
	if st.Status != StatusFailed {
		t.Fatalf("status = %s", st.Status)
	}
}

const verdictYML = `
name: v
start: validator
nodes:
  validator:
    type: agent
    session: resume
    model: llm-openai/gpt-6
    prompt: "review"
    verdict: true
    pass: done
    fail: fail
    max_visits: 1
`

func TestRunVerdict(t *testing.T) {
	for reply, want := range map[string]string{
		"all good\nVERDICT: PASS  \n": StatusPassed,
		"bad\nVERDICT: FAIL":          StatusFailed,
		"I think it passes":           StatusFailed,
	} {
		p, err := writePipeline(t, verdictYML, nil)
		if err != nil {
			t.Fatal(err)
		}
		f := newFake(func(string, string) string { return reply })
		r := newRun(t, p, f)
		st, _ := r.Run(context.Background())
		if st.Status != want {
			t.Errorf("reply %q: status %s, want %s", reply, st.Status, want)
		}
		if sent := f.sentTo(st.Sessions["validator"]); len(sent) != 2 || sent[0] != "/model llm-openai gpt-6" {
			t.Errorf("sent = %q", sent)
		}
	}
}

func TestRunResumeReusesSession(t *testing.T) {
	yml := strings.Replace(validYML, "    type: agent\n", "    type: agent\n    session: resume\n", 1)
	yml = strings.Replace(yml, `run: "exit 0"`, `run: "exit 1"`, 1)
	p, err := writePipeline(t, yml, nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(echo)
	st, _ := newRun(t, p, f).Run(context.Background())
	if f.createdCount() != 1 || len(f.sentTo(st.Sessions["coder"])) != 3 {
		t.Errorf("resume: created %d, sent %q", f.createdCount(), f.sentTo(st.Sessions["coder"]))
	}
}

func TestRunStopKillsChild(t *testing.T) {
	p, err := writePipeline(t, validYML, nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(func(string, string) string { return "HANG" })
	r := newRun(t, p, f)
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		for f.createdCount() == 0 {
			time.Sleep(5 * time.Millisecond)
		}
		time.Sleep(20 * time.Millisecond)
		cancel()
	}()
	st, _ := r.Run(ctx)
	if st.Status != StatusStopped {
		t.Fatalf("status = %s", st.Status)
	}
	if id := st.Sessions["coder"]; !slices.Contains(f.killed, id) || f.Live(id) {
		t.Errorf("child %s not killed (killed %v)", id, f.killed)
	}
	if saved, _ := ReadState(r.opt.Home, r.ID()); saved.Status != StatusStopped {
		t.Errorf("state.json status = %s", saved.Status)
	}
}

func TestRunPendingAskFailsVisit(t *testing.T) {
	old := askPoll
	askPoll = 10 * time.Millisecond
	defer func() { askPoll = old }()
	p, err := writePipeline(t, strings.Replace(validYML, "fail: coder\n    max_visits: 3\n  tests", "fail: fail\n    max_visits: 3\n  tests", 1), nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(func(string, string) string { return "ASK" })
	r := newRun(t, p, f)
	st, _ := r.Run(context.Background())
	if st.Status != StatusFailed {
		t.Fatalf("status = %s", st.Status)
	}
	res, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/1-coder/result.json"))
	if !strings.Contains(string(res), `"reason": "ask"`) || len(f.killed) == 0 {
		t.Errorf("result = %s, killed %v", res, f.killed)
	}
}

func TestRunLeakWithheld(t *testing.T) {
	yml := `
name: l
start: coder
nodes:
  coder:
    type: agent
    mode: project
    project: demo
    prompt: "fix: {{input}} {{run_dir}} {{holdout_dir}}"
    next: validator
    fail: coder
    max_visits: 2
  validator:
    type: agent
    holdout: [acceptance/*.md]
    prompt: "criteria in {{holdout_dir}}"
    verdict: true
    pass: done
    fail: coder
    max_visits: 1
`
	p, err := writePipeline(t, yml, map[string]string{"acceptance/a.md": criteria})
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(func(_, prompt string) string {
		if strings.HasPrefix(prompt, "criteria") {
			return "Missing: " + criteria + "\nVERDICT: FAIL"
		}
		return "done"
	})
	home := t.TempDir()
	writeProject(t, home, "demo", "repos:\n  - remote: https://example.com/x.git\n")
	r, err := NewRunner(p, Options{Home: home, Sessions: f})
	if err != nil {
		t.Fatal(err)
	}
	st, _ := r.Run(context.Background())
	if st.Status != StatusExhausted {
		t.Fatalf("status = %s", st.Status)
	}
	routed, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/2-validator/routed.md"))
	if string(routed) != withheld {
		t.Errorf("routed.md = %q", routed)
	}
	reply, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/2-validator/reply.md"))
	if !strings.Contains(string(reply), criteria) {
		t.Error("reply.md lost the full reply")
	}
	vprompt, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/2-validator/prompt.md"))
	if string(vprompt) != "criteria in "+filepath.Join(r.Dir(), "holdout") {
		t.Errorf("validator prompt = %q", vprompt)
	}
	cprompt, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/3-coder/prompt.md"))
	if strings.Contains(string(cprompt), "holdout") || strings.Contains(string(cprompt), "column order") || strings.Contains(string(cprompt), r.Dir()) || !strings.Contains(string(cprompt), withheld) {
		t.Errorf("coder prompt leaks run dir/holdout: %q", cprompt)
	}
	if res, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/2-validator/result.json")); !strings.Contains(string(res), `"route": "coder"`) {
		t.Errorf("leak changed the route: %s", res)
	}
	evs, _ := ReadEvents(home, r.ID())
	if !slices.ContainsFunc(evs, func(e Event) bool { return e.Kind == "leak" }) {
		t.Error("no leak event")
	}
}
