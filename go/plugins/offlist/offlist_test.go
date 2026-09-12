package offlist

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func write(t *testing.T, home, body string) {
	t.Helper()
	if err := os.MkdirAll(home, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(Path(home), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestMissingFileNothingOff(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	l := Load(home)
	if l == nil {
		t.Fatal("Load returned nil")
	}
	if l.Err() != nil {
		t.Fatalf("missing file reported an error: %v", l.Err())
	}
	if l.Off("skill", "circleci") {
		t.Fatal("nothing should be off")
	}
}

func TestEmptyFileNothingOff(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	write(t, home, "")
	l := Load(home)
	if l.Err() != nil {
		t.Fatalf("empty file reported an error: %v", l.Err())
	}
	if len(l.Entries()) != 0 {
		t.Fatalf("expected no entries, got %v", l.Entries())
	}
}

func TestListedAndUnlisted(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	write(t, home, "off:\n  - skill:circleci\n  - hook:post-result/audit.js\n  - plugin:uni-frontend@uni-claude-marketplace\n")
	l := Load(home)
	if l.Err() != nil {
		t.Fatal(l.Err())
	}
	for _, c := range [][2]string{
		{"skill", "circleci"},
		{"hook", "post-result/audit.js"},
		{"plugin", "uni-frontend@uni-claude-marketplace"},
	} {
		if !l.Off(c[0], c[1]) {
			t.Errorf("%s:%s should be off", c[0], c[1])
		}
	}
	if l.Off("skill", "exa") {
		t.Error("unlisted skill should be on")
	}
	if l.Off("rule", "circleci") {
		t.Error("kind must be part of the match")
	}
}

func TestSetRoundTrip(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	write(t, home, "off:\n  - skill:circleci\n  - watcher:ci.js\n")

	l := Load(home)
	if err := l.Set(home, "rule", "/Users/a/.claude/rules/python.md", true); err != nil {
		t.Fatal(err)
	}
	if !l.Off("rule", "/Users/a/.claude/rules/python.md") {
		t.Fatal("newly set item should be off in the receiver")
	}

	got := Load(home).Entries()
	want := []string{"skill:circleci", "watcher:ci.js", "rule:/Users/a/.claude/rules/python.md"}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("order not preserved: got %v want %v", got, want)
	}

	if err := l.Set(home, "skill", "circleci", false); err != nil {
		t.Fatal(err)
	}
	got = Load(home).Entries()
	want = []string{"watcher:ci.js", "rule:/Users/a/.claude/rules/python.md"}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("removal wrong: got %v want %v", got, want)
	}
	if Load(home).Off("skill", "circleci") {
		t.Fatal("removed item should be on again")
	}
}

func TestSetOnMissingFileCreatesIt(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	l := Load(home)
	if err := l.Set(home, "hook", "pre-tool/guard.js", true); err != nil {
		t.Fatal(err)
	}
	if !Load(home).Off("hook", "pre-tool/guard.js") {
		t.Fatal("expected the item off after Set on a fresh home")
	}
}

func TestMalformedReportsAndDisablesNothing(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	write(t, home, "off:\n  - skill:circleci\n   bad indent: [\n")
	l := Load(home)
	if l.Err() == nil {
		t.Fatal("malformed file must report through Err()")
	}
	if len(l.Entries()) != 0 || l.Off("skill", "circleci") {
		t.Fatal("malformed file must mean nothing off, never everything off")
	}
	if err := l.Set(home, "skill", "x", true); err == nil {
		t.Fatal("Set must refuse to overwrite an unparseable file")
	}
}

func TestWrongShapeIsMalformed(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	write(t, home, "off: skill:circleci\n")
	l := Load(home)
	if l.Err() == nil {
		t.Fatal("a scalar where a list belongs should report an error")
	}
	if l.Off("skill", "circleci") {
		t.Fatal("nothing should be off")
	}
}

func TestCacheSeesEdits(t *testing.T) {
	t.Parallel()
	home := filepath.Join(t.TempDir(), "bough")
	write(t, home, "off:\n  - skill:a\n")
	if !Load(home).Off("skill", "a") {
		t.Fatal("expected skill:a off")
	}
	write(t, home, "off:\n  - skill:bb\n")
	l := Load(home)
	if l.Off("skill", "a") || !l.Off("skill", "bb") {
		t.Fatalf("cache did not refresh: %v", l.Entries())
	}
}

// `off` is a YAML 1.1 boolean, so writing it as a key produces `"off":`
// and a config file nobody wants to hand-edit. The written key is
// `disabled`, and a file using the old `off` key still reads.
func TestDisabledKeyIsWrittenAndOffStillReads(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	l := Load(home)
	if err := l.Set(home, "skill", "circleci", true); err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(Path(home))
	if err != nil {
		t.Fatal(err)
	}
	if got := string(raw); !strings.Contains(got, "disabled:") || strings.Contains(got, `"off"`) {
		t.Errorf("written file is %q, want a plain disabled: key", got)
	}
	if !Load(home).Off("skill", "circleci") {
		t.Error("round-trip lost the entry")
	}

	// A hand-written or previously-written `off:` file still works.
	old := t.TempDir()
	if err := os.WriteFile(Path(old), []byte("off:\n  - skill:legacy\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if !Load(old).Off("skill", "legacy") {
		t.Error("the old off: key no longer reads")
	}
}
