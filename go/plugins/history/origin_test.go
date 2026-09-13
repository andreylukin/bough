package history

import (
	"path/filepath"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

func TestClassify(t *testing.T) {
	home := "/Users/me"
	cases := []struct {
		name, origin, cwd, prompt, want string
	}{
		{"web origin wins over temp cwd", "web", "/tmp/x", "/llm-wiki ingest a", ""},
		{"tui origin", "tui", "/Users/me/.bough/wiki", "", ""},
		{"headless origin", "headless", "/Users/me/repos/bough", "fix ci", "origin"},
		{"wiki dir", "", "/Users/me/.bough/wiki", "anything", "bough-home"},
		{"bough home itself", "", "/Users/me/.bough", "", "bough-home"},
		{"lookalike dir is not bough home", "", "/Users/me/.boughx", "", ""},
		{"tmp", "", "/tmp/bough-live-goal", "say hi", "temp-dir"},
		{"private tmp scratchpad", "", "/private/tmp/claude-501/x/scratchpad/live", "say hi", "temp-dir"},
		{"var folders", "", "/var/folders/ab/T/TestX/001", "", "temp-dir"},
		{"wiki prompt anywhere", "", "/Users/me", "/llm-wiki ingest 01a0", "wiki-ingest"},
		{"home repo stays the user's", "", "/Users/me/repos/bough", "tell me about the codebase", ""},
		{"no cwd stays the user's", "", "", "hi", ""},
		{"home dir stays the user's", "", "/Users/me", "", ""},
	}
	for _, c := range cases {
		if got := Classify(c.origin, c.cwd, c.prompt, home); got != c.want {
			t.Errorf("%s: Classify(%q, %q, %q) = %q, want %q", c.name, c.origin, c.cwd, c.prompt, got, c.want)
		}
	}
}

func applyWithOrigin(t *testing.T, origin, path string) *Store {
	t.Helper()
	ctx := kernel.NewContext()
	if origin != "" {
		ctx.Provide("origin", origin)
	}
	if err := (plugin{}).Apply(ctx, map[string]any{"file": path}); err != nil {
		t.Fatal(err)
	}
	s, err := kernel.Get[*Store](ctx, "history")
	if err != nil {
		t.Fatal(err)
	}
	s.Append("input", map[string]any{"text": "hi"})
	t.Cleanup(ctx.Unmount)
	return s
}

// The meta entry records who started the session.
func TestMetaRecordsOrigin(t *testing.T) {
	for _, o := range []string{"web", "tui", "headless"} {
		p := filepath.Join(t.TempDir(), o+".jsonl")
		s := applyWithOrigin(t, o, p)
		if es := s.Entries(); es[0].Kind != "meta" || es[0].Data["origin"] != o {
			t.Errorf("%s: meta = %+v", o, es[0].Data)
		}
		infos, err := List(filepath.Dir(p))
		if err != nil || len(infos) != 1 {
			t.Fatalf("List = %v, %v", infos, err)
		}
		if infos[0].Origin != o || infos[0].Background != (o == "headless") {
			t.Errorf("%s: info origin=%q background=%v", o, infos[0].Origin, infos[0].Background)
		}
	}
}

// A headless-born session a person resumes becomes theirs; one that is
// already theirs gains no entry.
func TestUserResumeClaimsBackgroundSession(t *testing.T) {
	p := filepath.Join(t.TempDir(), "bg.jsonl")
	applyWithOrigin(t, "headless", p).Close()

	s := applyWithOrigin(t, "tui", p)
	var claims int
	for _, e := range s.Entries() {
		if e.Kind == "origin" {
			claims++
		}
	}
	if claims != 1 {
		t.Fatalf("origin entries after a TUI resume = %d, want 1", claims)
	}
	s.Close()
	infos, _ := List(filepath.Dir(p))
	if infos[0].Origin != "tui" || infos[0].Background {
		t.Fatalf("after claim: origin=%q background=%v", infos[0].Origin, infos[0].Background)
	}

	s = applyWithOrigin(t, "web", p)
	for _, e := range s.Entries() {
		if e.Kind == "origin" {
			claims--
		}
	}
	if claims != 0 {
		t.Fatalf("resuming a session already the user's added an origin entry")
	}

	// A headless resume never claims.
	q := filepath.Join(t.TempDir(), "bg2.jsonl")
	applyWithOrigin(t, "headless", q).Close()
	for _, e := range applyWithOrigin(t, "headless", q).Entries() {
		if e.Kind == "origin" {
			t.Fatal("headless resume wrote an origin entry")
		}
	}
}
