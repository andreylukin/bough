package orb

import (
	"strings"
	"testing"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

// Every session in a project used to read the same paragraph, so the
// main thread had no reason to hand anything to a thread and did every
// job itself: the project page's thread list stayed empty and the
// status groups it is built around never populated. A thread, in turn,
// was told to "ask the main thread" without ever being told it was a
// thread or which session main is.
func TestPromptSectionSaysWhichSessionThisIs(t *testing.T) {
	t.Parallel()
	st := iorb.State{Project: "web", Container: "bough-orb-s1", Status: iorb.StatusRunning}

	main := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{main: true})
	for _, want := range []string{"MAIN THREAD of web", "tools.spawn", "background", "project page"} {
		if !strings.Contains(main, want) {
			t.Errorf("the main thread's section lacks %q:\n%s", want, main)
		}
	}

	thread := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{parent: "sess-main"})
	for _, want := range []string{"THREAD of web", "sess-main", "cannot start threads"} {
		if !strings.Contains(thread, want) {
			t.Errorf("a thread's section lacks %q:\n%s", want, thread)
		}
	}
	if strings.Contains(thread, "MAIN THREAD") {
		t.Errorf("a thread was told it is the main thread:\n%s", thread)
	}

	// A thread a person started from the project page has main as its
	// parent only for its report: it was told it "cannot start threads"
	// and so never delegated at all.
	person := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{thread: true, parent: "sess-main"})
	for _, want := range []string{"THREAD of web", "project page", "subagents", "background agents"} {
		if !strings.Contains(person, want) {
			t.Errorf("a person's thread's section lacks %q:\n%s", want, person)
		}
	}
	if strings.Contains(person, "cannot start") || strings.Contains(person, "MAIN THREAD") {
		t.Errorf("a person's thread was told it cannot delegate, or is main:\n%s", person)
	}

	// A session started from the CLI is neither, and is told neither.
	plain := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{})
	if strings.Contains(plain, "MAIN THREAD") || strings.Contains(plain, "THREAD of web") {
		t.Errorf("a session with no parent was given a role:\n%s", plain)
	}
}
