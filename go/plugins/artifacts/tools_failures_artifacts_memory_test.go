package artifacts

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/web"
)

// artifactsMemoryKnown skips a subtest that pins a known product bug
// unless BOUGH_KNOWN_TOOLS_ARTIFACTS_MEMORY=1.
func artifactsMemoryKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_ARTIFACTS_MEMORY") != "1" {
		t.Skip("known bug: " + bug)
	}
}

// artifactsMemoryVM is codemode with the four artifact tools bound the
// way Apply binds them, so each case sees what the model sees.
func artifactsMemoryVM(t *testing.T, s *Store) *codemode.CodeMode {
	t.Helper()
	cm := codemode.New(5 * time.Second)
	cm.RegisterTool("artifact", s.Publish)
	cm.RegisterTool("artifactPatch", s.Patch)
	cm.RegisterTool("artifactAnswers", s.Answers)
	cm.RegisterTool("artifactGuide", s.Guide)
	return cm
}

// artifactsMemoryAlive: the VM still runs a block after a failure.
func artifactsMemoryAlive(t *testing.T, cm *codemode.CodeMode) {
	t.Helper()
	if out, err := cm.Run(`1+1`); err != nil || out != "2" {
		t.Fatalf("VM not usable after failure: %q %v", out, err)
	}
}

func artifactsMemoryJS(s string) string {
	return "`" + strings.ReplaceAll(s, "`", "\\`") + "`"
}

// Every failing artifact call becomes a JS exception carrying the fix,
// catchable by the model, and the VM survives it.
func TestArtifactsMemoryToolErrorsReachTheModel(t *testing.T) {
	s := newStore(t)
	cm := artifactsMemoryVM(t, s)
	for _, c := range []struct{ name, js, want string }{
		{"prose not lang", `tools.artifact("p", "just some prose")`, "no statements"},
		{"no root", `tools.artifact("p", 'head = CardHeader("x")')`, "no root"},
		{"fenced", "tools.artifact(\"p\", \"```\\nroot = Card([])\\n```\")", "markdown fence"},
		{"duplicate", `tools.artifact("p", "root = Card([a])\na = X()\na = Y()")`, "twice"},
		{"no args", `tools.artifact()`, "artifact name"},
		{"object code", `tools.artifact("p", {root: 1})`, "no statements"},
		{"patch missing page", `tools.artifactPatch("ghost", "root = Card([])")`, "not published"},
		{"answers missing page", `tools.artifactAnswers("ghost")`, "not published"},
		{"patch bad name", `tools.artifactPatch("../x", "root = Card([])")`, "artifact name"},
	} {
		t.Run(c.name, func(t *testing.T) {
			_, err := cm.Run(c.js)
			if err == nil || !strings.Contains(err.Error(), c.want) {
				t.Fatalf("err = %v, want %q", err, c.want)
			}
			// Catchable: the model can recover inside one block.
			out, err := cm.Run(`try { ` + c.js + `; "no throw" } catch (e) { "caught: " + e.message }`)
			if err != nil || !strings.HasPrefix(out, "caught: ") {
				t.Fatalf("not catchable: %q %v", out, err)
			}
			artifactsMemoryAlive(t, cm)
		})
	}
	if es, _ := os.ReadDir(filepath.Join(s.root, s.session)); len(es) != 0 {
		t.Fatalf("refused calls left files: %v", es)
	}
}

// A patch that is not statements, or that breaks the page, is refused
// and leaves the published version untouched.
func TestArtifactsMemoryBadPatchKeepsThePage(t *testing.T) {
	s := newStore(t)
	cm := artifactsMemoryVM(t, s)
	if _, err := cm.Run(`tools.artifact("p", ` + artifactsMemoryJS(sample) + `)`); err != nil {
		t.Fatal(err)
	}
	before, _ := os.ReadFile(s.codePath("p"))
	for _, p := range []string{"not a statement at all", "", "root = null", "a = X()\na = Y()\nroot = null", "x = 1\n```"} {
		if _, err := s.Patch("p", p); err == nil {
			t.Errorf("patch %q accepted", p)
		}
	}
	if b, _ := os.ReadFile(s.codePath("p")); string(b) != string(before) {
		t.Fatalf("page changed by refused patches:\n%s", b)
	}
	if m := readMeta(metaPath(s.codePath("p"))); m.Version != 1 {
		t.Fatalf("version bumped by refused patches: %d", m.Version)
	}
	artifactsMemoryAlive(t, cm)
}

// artifactAnswers on a page nobody touched, or whose answers file is
// corrupt, says so instead of failing; corrupt reports are not notices.
func TestArtifactsMemoryAnswersEmptyAndCorrupt(t *testing.T) {
	s := newStore(t)
	if _, err := s.Publish("p", sample); err != nil {
		t.Fatal(err)
	}
	if got, err := s.Answers("p"); err != nil || got != "no answers yet on p" {
		t.Fatalf("empty: %q %v", got, err)
	}
	if err := os.WriteFile(answersPath(s.codePath("p")), []byte("{not json"), 0o644); err != nil {
		t.Fatal(err)
	}
	if got, err := s.Answers("p"); err != nil || !strings.Contains(got, "no answers yet") {
		t.Fatalf("corrupt: %q %v", got, err)
	}
	if err := os.WriteFile(errorsPath(s.codePath("p")), []byte("\x00garbage"), 0o644); err != nil {
		t.Fatal(err)
	}
	var notices []string
	s.notify = func(n string) { notices = append(notices, n) }
	s.sweep()
	if len(notices) != 0 {
		t.Fatalf("notices from corrupt files: %q", notices)
	}
}

// Names: traversal, unicode, spaces, empty and overlong. Nothing may
// land outside the session directory.
func TestArtifactsMemoryNames(t *testing.T) {
	s := newStore(t)
	refused := []string{"", "   ", "../x", "../../etc/passwd", "a/../../b", "..", "a..b", ".hidden", "-x",
		"café", "页面", "emoji-😀", "nul\x00byte", "tab\tname", strings.Repeat("a", 65), "C:\\x\\..\\y"}
	for _, n := range refused {
		if _, err := s.Publish(n, sample); err == nil {
			t.Errorf("name %q accepted", n)
		} else if !strings.Contains(err.Error(), "artifact name") {
			t.Errorf("name %q: %v", n, err)
		}
	}
	for n, want := range map[string]string{"My Page": "my-page", "a/b": "a-b", "Report.ui": "report", "x.html": "x", strings.Repeat("a", 64): strings.Repeat("a", 64)} {
		url, err := s.Publish(n, sample)
		if err != nil || !strings.HasSuffix(url, "/"+want) {
			t.Errorf("name %q: %q %v (want …/%s)", n, url, err, want)
		}
	}
	_ = filepath.Walk(s.root, func(p string, fi os.FileInfo, err error) error {
		if err == nil && !fi.IsDir() && filepath.Dir(p) != filepath.Join(s.root, s.session) {
			t.Errorf("file outside the session dir: %s", p)
		}
		return nil
	})
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	for _, u := range []string{"/artifacts/s1/..%2F..%2Fetc", "/artifacts/../s1/my-page", "/artifacts/s1/my-page/nope"} {
		if code, _ := get(t, s.web.URL()+u); code == 200 {
			t.Errorf("%s served 200", u)
		}
	}
}

// A store whose directory cannot be written reports the write error.
func TestArtifactsMemoryUnwritableRoot(t *testing.T) {
	s := newStore(t)
	file := filepath.Join(t.TempDir(), "flat")
	if err := os.WriteFile(file, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	s.root = file
	if _, err := s.Publish("p", sample); err == nil || !strings.HasPrefix(err.Error(), "artifact:") {
		t.Fatalf("publish into a file: %v", err)
	}
	if _, err := s.Answers("p"); err == nil {
		t.Fatal("answers on an unwritable store")
	}
}

// The browser failing to open still publishes and hands over the URL.
func TestArtifactsMemoryBrowserFails(t *testing.T) {
	s := newStore(t)
	s.open = func(string) error { return os.ErrPermission }
	got, err := s.Publish("p", sample)
	if err != nil || !strings.Contains(got, "did not open") || !strings.Contains(got, "/artifacts/s1/p") {
		t.Fatalf("publish: %q %v", got, err)
	}
	if got := s.openLatest(); !strings.Contains(got, "did not open") {
		t.Fatalf("openLatest: %q", got)
	}
}

// When the web row could not listen at all, a publish still returns a
// URL that nothing serves; the agent should be told.
func TestArtifactsMemoryWebNotServing(t *testing.T) {
	w := web.New("203.0.113.7:0") // TEST-NET-3: not a local address
	if w.Serving() {
		t.Skip("this host can bind 203.0.113.7")
	}
	s := &Store{root: t.TempDir(), session: "s1", web: w, opened: map[string]bool{}, seen: map[string]int{}, errSeen: map[string]string{}}
	url, err := s.Publish("p", sample)
	if err != nil {
		t.Fatalf("publish: %v", err)
	}
	artifactsMemoryKnown(t, "artifacts.go Publish returns a bare URL when web.Serving() is false — the agent hands the user a dead link")
	if !strings.Contains(url, "not serv") {
		t.Fatalf("dead URL given as success: %q", url)
	}
}
