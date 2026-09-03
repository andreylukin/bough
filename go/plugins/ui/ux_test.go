package ui

// Wave-2 UX contracts: the fresh-session welcome text, error wrapping
// (with the credential hint on auth-shaped failures), and the todo
// event's one-render-per-mutation rule.

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/plugins/commands"
)

// --- welcome ---

const welcomeLine = "type / for commands"

func TestWelcomeShownOnFreshSession(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t) // no history service at all
	p := d.plain()
	for _, want := range []string{
		"bough — a coding agent",
		welcomeLine,
		"ask me to do something — I act by running code",
	} {
		if !strings.Contains(p, want) {
			t.Errorf("fresh session should show %q:\n%s", want, p)
		}
	}
}

func TestWelcomeShownWithEmptyHistory(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, fakeHist{path: "/tmp/s.jsonl"}))
	if !strings.Contains(d.plain(), welcomeLine) {
		t.Errorf("0-entry history is a fresh session; welcome missing:\n%s", d.plain())
	}
}

func TestWelcomeSuppressedOnResume(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, histWith("/tmp/s.jsonl", "prior question")))
	if strings.Contains(d.plain(), welcomeLine) {
		t.Errorf("a resumed session must not show the welcome text:\n%s", d.plain())
	}
}

func TestWelcomeGoneAfterFirstSubmit(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("hello")
	d.press(keyEnter())
	p := d.plain()
	if strings.Contains(p, welcomeLine) {
		t.Errorf("welcome must be gone after the first turn:\n%s", p)
	}
	if !strings.Contains(p, "❯ hello") {
		t.Errorf("user block missing:\n%s", p)
	}
}

func TestWelcomeGoneOnFirstEvent(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "hi from the shared loop")
	if strings.Contains(d.plain(), welcomeLine) {
		t.Errorf("any transcript content hides the welcome:\n%s", d.plain())
	}
}

func TestWelcomeClearRemovesForGood(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, uiActionReg(t, map[string]commands.UIAction{"clear": commands.ActionClear}))
	if !strings.Contains(d.plain(), welcomeLine) {
		t.Fatalf("precondition: welcome shown:\n%s", d.plain())
	}
	d.typeStr("/clear")
	d.press(keyEnter())
	if len(d.m.blocks) != 0 {
		t.Fatalf("/clear should empty the transcript, %d blocks left", len(d.m.blocks))
	}
	if strings.Contains(d.plain(), welcomeLine) {
		t.Errorf("/clear removes the welcome and it stays gone:\n%s", d.plain())
	}
}

// --- error wrap + credential hint ---

func TestErrorRenderWrapsLongLine(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t) // width 80
	long := strings.Repeat("x", 290) + " TAIL_MARKER"
	out := stripANSI(d.m.render(&block{kind: "error", text: long}, d.m.cfg.Load()))
	for i, line := range strings.Split(out, "\n") {
		if n := len([]rune(line)); n > 80 {
			t.Errorf("error line %d is %d cols wide (max 80): %q", i, n, line)
		}
	}
	if !strings.Contains(strings.ReplaceAll(out, "\n", ""), "TAIL_MARKER") {
		t.Errorf("wrapped error lost its tail:\n%s", out)
	}
}

func TestErrorTailVisibleOnScreen(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("error", strings.Repeat("x", 290)+" TAIL_MARKER")
	if !strings.Contains(d.plain(), "TAIL_MARKER") {
		t.Errorf("the end of a long error must be on screen, not clipped:\n%s", d.plain())
	}
}

func TestAuthErrorAppendsCredentialHint(t *testing.T) {
	t.Parallel()
	for _, text := range []string{
		"POST https://api.anthropic.com/v1/messages: 401 Unauthorized",
		"invalid x-api-key",
		"missing API key",
	} {
		d := defaultDrv(t)
		d.event("error", text)
		p := d.plain()
		for _, want := range []string{"hint: check", "ANTHROPIC_API_KEY", "/model"} {
			if !strings.Contains(p, want) {
				t.Errorf("error %q should carry the credential hint (%q):\n%s", text, want, p)
			}
		}
	}
}

func TestPlainErrorNoCredentialHint(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("error", "loop: gave up after 10 steps")
	if strings.Contains(d.plain(), "hint:") {
		t.Errorf("a non-auth error must not carry the credential hint:\n%s", d.plain())
	}
}

// --- todo: one render per mutation ---

func TestTodoEventDedicatedRender(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("todo", "[ ] 1 buy milk\n[x] 2 done thing")
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "todo" {
		t.Fatalf("want one todo block, got %+v", d.m.blocks)
	}
	p := d.plain()
	if !strings.Contains(p, "[ ] 1 buy milk") || !strings.Contains(p, "[x] 2 done thing") {
		t.Errorf("checkbox lines missing:\n%s", p)
	}
	// The dedicated render puts the tag on its own line — not the raw
	// unknown-kind fallback's inline "todo [ ] ...".
	if strings.Contains(p, "todo [ ]") {
		t.Errorf("todo event still renders through the raw fallback:\n%s", p)
	}
}

func TestTodoConsecutiveEventsUpdateInPlace(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("todo", "[ ] 1 a")
	d.event("todo", "[ ] 1 a\n[ ] 2 b")
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "todo" {
		t.Fatalf("consecutive todo events must collapse into one block, got %+v", d.m.blocks)
	}
	if n := strings.Count(d.plain(), "[ ] 1 a"); n != 1 {
		t.Errorf("list rendered %d times, want 1:\n%s", n, d.plain())
	}
}

func TestTodoCommandMutationRendersOnce(t *testing.T) {
	t.Parallel()
	list := "[ ] 1 buy milk"
	r := commands.NewRegistry()
	if err := r.Register(commands.CommandInfo{Name: "todo", Summary: "todos"},
		func(string) (string, error) { return list, nil }); err != nil {
		t.Fatal(err)
	}
	d := drvCmds(t, r)
	d.typeStr("/todo add buy milk")
	d.press(keyEnter())
	// The mutation's todo event arrives after dispatch printed the
	// system block with the same rendered list.
	d.event("todo", list)
	if n := strings.Count(d.plain(), "buy milk"); n != 2 { // command echo + one list render
		t.Errorf("list should render once (plus the command echo), buy milk appears %d times:\n%s", n, d.plain())
	}
	last := d.m.blocks[len(d.m.blocks)-1]
	if last.kind != "todo" || last.text != list {
		t.Errorf("the system block should have become the todo block, got %+v", d.m.blocks)
	}
	if len(d.m.blocks) != 2 { // command echo + todo
		t.Errorf("want 2 blocks (echo + todo), got %+v", d.m.blocks)
	}
}

func TestTodoEventAfterCodeAppends(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("code", "tools.todo.add('x')")
	d.event("todo", "[ ] 1 x")
	if len(d.m.blocks) != 2 || d.m.blocks[0].kind != "code" || d.m.blocks[1].kind != "todo" {
		t.Fatalf("a code-driven mutation appends the todo block, got %+v", d.m.blocks)
	}
}

// The launcher's "notice" (a stale dev binary) is the first thing on
// screen, as an error row, with or without a history service.
func TestLaunchNoticeShowsAsErrorRow(t *testing.T) {
	t.Parallel()
	for _, hist := range []historyView{nil, histWith("/tmp/h.jsonl")} {
		cfg := cfgWith(t, nil, nil, hist)
		cfg.notice = "this binary was built from 11111111; the checkout is at 22222222 — run `bough update`"
		d := newDrv(t, 80, 24, cfg)
		if p := d.plain(); !strings.Contains(p, "✗ this binary was built from 11111111") {
			t.Fatalf("notice missing (hist=%v):\n%s", hist != nil, p)
		}
	}
}
