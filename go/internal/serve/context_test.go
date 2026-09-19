package serve

import (
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/contextmd"
	"github.com/andreylukin/bough/plugins/history"
)

func write(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// seedAt seeds a session whose meta says it works in cwd.
func seedAt(t *testing.T, f *apiFixture, id, cwd string) {
	t.Helper()
	f.seed(t, id, history.Entry{
		Seq: 1, At: time.Now(), Kind: "meta",
		Data: map[string]any{"cwd": cwd},
	})
}

func TestSessionContext(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	work := filepath.Join(f.home, "work")
	write(t, filepath.Join(work, "AGENTS.md"), "## Testing\n\nRun make test.\n")
	write(t, filepath.Join(work, "CLAUDE.md"), "## Testing\n\nRun make test.\n\n## Extra\n\nUse tabs.\n")
	write(t, filepath.Join(work, ".claude", "rules", "python.md"), "---\npaths:\n  - \"**/*.py\"\n---\nbe terse\n")
	seedAt(t, f, "s1", work)

	code, body := f.do(t, "GET", "/api/sessions/s1/context", "")
	if code != http.StatusOK {
		t.Fatalf("GET context = %d %v", code, body)
	}
	if body["cwd"] != work {
		t.Fatalf("cwd = %v, want the session's %q", body["cwd"], work)
	}

	files, _ := body["contextFiles"].([]any)
	if len(files) != len(contextmd.Paths(f.home, "")) {
		t.Fatalf("contextFiles = %v", body["contextFiles"])
	}
	agents, _ := files[0].(map[string]any)
	claude, _ := files[1].(map[string]any)
	if agents["path"] != filepath.Join(work, "AGENTS.md") || agents["found"] != true {
		t.Errorf("AGENTS.md row = %v", agents)
	}
	// The section CLAUDE.md repeated is reported, with the file that
	// said it first.
	if claude["dropped"] != float64(1) || claude["same"] != agents["path"] {
		t.Errorf("CLAUDE.md row = %v", claude)
	}
	if home, _ := files[2].(map[string]any); home["found"] != false {
		t.Errorf("~/.claude/CLAUDE.md row = %v, want not found", home)
	}
	// Length is what a context file costs on every turn, so it is on
	// the row; a file that is not there has no length.
	if agents["lines"] != float64(3) {
		t.Errorf("AGENTS.md lines = %v, want 3", agents["lines"])
	}
	if home, _ := files[2].(map[string]any); home["lines"] != float64(0) {
		t.Errorf("missing file lines = %v, want 0", home["lines"])
	}

	rules, _ := body["rules"].([]any)
	if len(rules) != 1 {
		t.Fatalf("rules = %v, want the project rule", body["rules"])
	}
	rule, _ := rules[0].(map[string]any)
	if rule["name"] != "python.md" || rule["kind"] != "scoped" || rule["scope"] != "repo" || rule["off"] != false {
		t.Errorf("rule = %v", rule)
	}
	if globs, _ := rule["globs"].([]any); len(globs) != 1 || globs[0] != "**/*.py" {
		t.Errorf("globs = %v", rule["globs"])
	}
	if _, ok := body["skills"].([]any); !ok {
		t.Errorf("skills = %v, want a list", body["skills"])
	}
}

// The rules of a session are the rules of ITS directory: serve's own
// cwd is wherever serve was started, which is the wrong answer.
func TestSessionContextUsesSessionCwd(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	a := filepath.Join(f.home, "a")
	b := filepath.Join(f.home, "b")
	write(t, filepath.Join(a, ".claude", "rules", "a.md"), "rule a\n")
	write(t, filepath.Join(b, ".claude", "rules", "b.md"), "rule b\n")
	seedAt(t, f, "sa", a)
	seedAt(t, f, "sb", b)

	for id, want := range map[string]string{"sa": "a.md", "sb": "b.md"} {
		_, body := f.do(t, "GET", "/api/sessions/"+id+"/context", "")
		rules, _ := body["rules"].([]any)
		if len(rules) != 1 {
			t.Fatalf("%s: rules = %v", id, body["rules"])
		}
		if row, _ := rules[0].(map[string]any); row["name"] != want {
			t.Errorf("%s: rule = %v, want %s", id, row, want)
		}
	}
}

// Nothing on disk is an empty list, never a JSON null.
func TestSessionContextEmptyLists(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	work := filepath.Join(f.home, "bare")
	if err := os.MkdirAll(work, 0o755); err != nil {
		t.Fatal(err)
	}
	seedAt(t, f, "s1", work)

	_, body := f.do(t, "GET", "/api/sessions/s1/context", "")
	for _, key := range []string{"rules", "contextFiles", "skills"} {
		list, ok := body[key].([]any)
		if !ok {
			t.Fatalf("%s = %v, want a list", key, body[key])
		}
		if key == "rules" && len(list) != 0 {
			t.Errorf("rules = %v, want none", list)
		}
	}
}

func TestSessionContextUnknownSession(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	if code, _ := f.do(t, "GET", "/api/sessions/nope/context", ""); code != http.StatusNotFound {
		t.Fatalf("unknown session = %d, want 404", code)
	}
}

// A session in a project is told the project's MEMORY.md first, and
// being first is what makes it the file credited for a section that
// the repo's AGENTS.md repeats.
func TestSessionContextProjectMemory(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	slug := mkProject(t, f, "Web")
	mem := filepath.Join(f.home, ".bough", "projects", slug, "MEMORY.md")
	write(t, mem, "## Deploy\n\nship on green.\n")
	work := filepath.Join(f.home, "work")
	write(t, filepath.Join(work, "AGENTS.md"), "## Deploy\n\nship on green.\n\n## Testing\n\nrun make.\n")
	seedAt(t, f, "s1", work)
	if code, body := f.do(t, "POST", "/api/sessions/s1/project", `{"project":"`+slug+`"}`); code != http.StatusOK {
		t.Fatalf("assign = %d %v", code, body)
	}

	_, body := f.do(t, "GET", "/api/sessions/s1/context", "")
	files, _ := body["contextFiles"].([]any)
	if len(files) != len(contextmd.Paths(f.home, slug)) {
		t.Fatalf("contextFiles = %v", body["contextFiles"])
	}
	first, _ := files[0].(map[string]any)
	if first["path"] != mem || first["found"] != true || first["lines"] != float64(3) {
		t.Fatalf("first row = %v, want %s read first", first, mem)
	}
	// MEMORY.md said it, so AGENTS.md is the copy that is dropped.
	agents, _ := files[1].(map[string]any)
	if agents["dropped"] != float64(1) || agents["same"] != mem {
		t.Errorf("AGENTS.md row = %v, want its section credited to MEMORY.md", agents)
	}
}

// The count the control room colours is the count of lines on disk: a
// trailing newline ends the last line, it does not start another.
func TestSessionContextLineCount(t *testing.T) {
	t.Parallel()
	if longLines >= tooLongLines {
		t.Fatalf("thresholds %d/%d are the wrong way round", longLines, tooLongLines)
	}
	lines := func(n int, nl bool) string {
		body := ""
		for i := 0; i < n; i++ {
			body += fmt.Sprintf("line %d\n", i)
		}
		if !nl {
			body = strings.TrimSuffix(body, "\n")
		}
		return body
	}
	f := newHooksAPI(t)
	work := filepath.Join(f.home, "long")
	write(t, filepath.Join(work, "AGENTS.md"), lines(longLines+1, true))
	write(t, filepath.Join(work, "CLAUDE.md"), lines(tooLongLines+1, false))
	seedAt(t, f, "s1", work)

	_, got := f.do(t, "GET", "/api/sessions/s1/context", "")
	files, _ := got["contextFiles"].([]any)
	for i, want := range []int{longLines + 1, tooLongLines + 1} {
		row, _ := files[i].(map[string]any)
		if row["lines"] != float64(want) {
			t.Errorf("%v lines = %v, want %d", row["path"], row["lines"], want)
		}
	}
}
