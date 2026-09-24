package e2e

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// loopRunDir is the one run under home whose state names this pipeline.
func loopRunDirs(t *testing.T, home string) []string {
	t.Helper()
	m, _ := filepath.Glob(filepath.Join(home, ".bough", "loops", "*", "state.json"))
	var out []string
	for _, p := range m {
		out = append(out, filepath.Dir(p))
	}
	return out
}

const loopLocalYML = `
name: local-echo
goal: create the marker
start: coder
max_steps: 10
nodes:
  coder:
    type: agent
    prompt: "visit {{visit}}: {{goal}}"
    next: tests
    fail: fail
    max_visits: 3
  tests:
    type: check
    # The check itself creates the marker on its second visit.
    run: "if [ -f seen ]; then touch marker; fi; touch seen; test -f marker"
    pass: done
    fail: FAILTARGET
    max_visits: 3
`

// A local-only pipeline with llm-echo: the check fails into fail (exit
// 1), and with fail routed back to the coder it passes on the second
// visit (exit 0). Project mode is not covered here: e2e fakes no orbs.
func TestLoopLocalEchoCheck(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{})
	extra := []string{"BOUGH_WEB_ADDR=127.0.0.1:0"}

	failDir := filepath.Join(cwd, "a")
	writeTree(t, failDir, map[string]string{"pipeline.yml": strings.Replace(loopLocalYML, "FAILTARGET", "fail", 1)})
	copyConfig(t, cwd, failDir)
	out, code := runCLIEnv(t, home, failDir, extra, "loop", "run", "pipeline.yml", "--set", "llm.plugin=llm-echo")
	if code != 1 {
		t.Fatalf("failing run exit = %d, want 1\n%s", code, out)
	}

	passDir := filepath.Join(cwd, "b")
	writeTree(t, passDir, map[string]string{"pipeline.yml": strings.Replace(loopLocalYML, "FAILTARGET", "coder", 1)})
	copyConfig(t, cwd, passDir)
	out, code = runCLIEnv(t, home, passDir, extra, "loop", "run", "pipeline.yml", "--set", "llm.plugin=llm-echo")
	if code != 0 {
		t.Fatalf("passing run exit = %d, want 0\n%s", code, out)
	}

	dirs := loopRunDirs(t, home)
	if len(dirs) != 2 {
		t.Fatalf("run dirs = %v", dirs)
	}
	var passed string
	for _, d := range dirs {
		b, _ := os.ReadFile(filepath.Join(d, "state.json"))
		var st struct{ ID, Status string }
		json.Unmarshal(b, &st)
		if st.Status == "passed" {
			passed = d
		}
	}
	if passed == "" {
		t.Fatalf("no passed run among %v", dirs)
	}
	for _, name := range []string{"pipeline.yml", "events.jsonl", "steps/1-coder/prompt.md", "steps/1-coder/reply.md",
		"steps/2-tests/output.txt", "steps/2-tests/result.json", "steps/3-coder/prompt.md", "steps/4-tests/result.json"} {
		if _, err := os.Stat(filepath.Join(passed, name)); err != nil {
			t.Errorf("run dir: %v", err)
		}
	}
	reply, _ := os.ReadFile(filepath.Join(passed, "steps/1-coder/reply.md"))
	if !strings.Contains(string(reply), "visit 1: create the marker") {
		t.Errorf("reply.md = %q", reply)
	}
	// The child is a normal history session with origin loop.
	hist, _ := filepath.Glob(filepath.Join(home, ".bough", "history", "*.jsonl"))
	if len(hist) == 0 {
		t.Error("no child session in history")
	} else if b, _ := os.ReadFile(hist[0]); !strings.Contains(string(b), `"origin":"loop"`) {
		t.Errorf("child meta has no loop origin: %.300s", b)
	}

	out, code = runCLIEnv(t, home, cwd, extra, "loop", "status")
	if code != 0 || !strings.Contains(out, "local-echo") || !strings.Contains(out, "passed") || !strings.Contains(out, "failed") {
		t.Errorf("status (%d):\n%s", code, out)
	}
	id := filepath.Base(passed)
	out, code = runCLIEnv(t, home, cwd, extra, "loop", "status", id)
	if code != 0 || !strings.Contains(out, "status:   passed") || !strings.Contains(out, "end") {
		t.Errorf("status %s (%d):\n%s", id, code, out)
	}
}

func copyConfig(t *testing.T, from, to string) {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(from, "bough.yml"))
	if err != nil {
		t.Fatal(err)
	}
	writeTree(t, to, map[string]string{"bough.yml": string(b)})
}

// writeTape writes a one-turn replay tape whose reply is text.
func writeTape(t *testing.T, path, text string) {
	t.Helper()
	now := time.Now()
	var b strings.Builder
	for i, e := range []map[string]any{
		{"kind": "meta", "data": map[string]any{"cwd": filepath.Dir(path)}},
		{"kind": "input", "data": map[string]any{"text": "review"}},
		{"kind": "assistant", "data": map[string]any{"text": text}},
		{"kind": "done", "data": map[string]any{}},
	} {
		e["seq"] = i + 1
		e["at"] = now
		line, _ := json.Marshal(e)
		b.Write(append(line, '\n'))
	}
	if err := os.WriteFile(path, []byte(b.String()), 0o644); err != nil {
		t.Fatal(err)
	}
}

const loopVerdictYML = `
name: verdict
start: validator
nodes:
  validator:
    type: agent
    prompt: "review"
    verdict: true
    pass: done
    fail: fail
    max_visits: 1
`

func TestLoopVerdictReplay(t *testing.T) {
	t.Parallel()
	home, cwd, _ := sandbox(t, launchOpts{cwd: map[string]string{"pipeline.yml": loopVerdictYML}})
	extra := []string{"BOUGH_WEB_ADDR=127.0.0.1:0"}
	for _, c := range []struct {
		reply string
		code  int
	}{
		{"The diff drops the header row.\nVERDICT: FAIL", 1},
		{"Everything checks out.\nVERDICT: PASS", 0},
	} {
		tape := filepath.Join(t.TempDir(), "tape.jsonl")
		writeTape(t, tape, c.reply)
		out, code := runCLIEnv(t, home, cwd, extra, "loop", "run", "pipeline.yml", "--set", "loop.plugin=loop", "--set", "llm.plugin=replay", "--set", "llm.file="+tape)
		if code != c.code {
			t.Errorf("tape %q: exit %d, want %d\n%s", c.reply, code, c.code, out)
		}
	}
}
