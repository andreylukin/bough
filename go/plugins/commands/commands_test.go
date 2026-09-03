package commands

import (
	"errors"
	"sort"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

func text(out string) func(string) (string, error) {
	return func(string) (string, error) { return out, nil }
}

func TestListSorted(t *testing.T) {
	r := NewRegistry()
	for _, n := range []string{"zeta", "alpha", "monarch"} {
		if err := r.Register(CommandInfo{Name: n}, text("x")); err != nil {
			t.Fatal(err)
		}
	}
	infos := r.List()
	if len(infos) != 3 {
		t.Fatalf("List len = %d, want 3", len(infos))
	}
	if !sort.SliceIsSorted(infos, func(i, j int) bool { return infos[i].Name < infos[j].Name }) {
		t.Fatalf("List not sorted: %v", infos)
	}
}

func TestRunUnknownCommand(t *testing.T) {
	r := NewRegistry()
	_, err := r.Run("nope", "")
	if err == nil || err.Error() != "unknown command: /nope (try /help)" {
		t.Fatalf("unknown command error = %v", err)
	}
}

func TestRunEmptyOutputEchoesName(t *testing.T) {
	r := NewRegistry()
	if err := r.Register(CommandInfo{Name: "silent"}, text("")); err != nil {
		t.Fatal(err)
	}
	out, err := r.Run("silent", "")
	if err != nil || out != "/silent" {
		t.Fatalf("Run = (%q, %v), want (/silent, nil)", out, err)
	}
}

func TestUIActionRoundTrip(t *testing.T) {
	r := NewRegistry()
	if err := r.Register(CommandInfo{Name: "boom"}, uiAction(ActionClear)); err != nil {
		t.Fatal(err)
	}
	out, err := r.Run("boom", "")
	if out != "" || err == nil {
		t.Fatalf("Run = (%q, %v), want empty output and sentinel error", out, err)
	}
	var a UIAction
	if !errors.As(err, &a) || a != ActionClear {
		t.Fatalf("errors.As UIAction = (%v, %v), want ActionClear", a, err)
	}
}

func TestRegisterRejectsBadAndDuplicate(t *testing.T) {
	r := NewRegistry()
	for _, bad := range []string{"", "/lead", "two words"} {
		if err := r.Register(CommandInfo{Name: bad}, text("x")); err == nil {
			t.Fatalf("Register(%q) succeeded, want error", bad)
		}
	}
	if err := r.Register(CommandInfo{Name: "once"}, text("x")); err != nil {
		t.Fatal(err)
	}
	if err := r.Register(CommandInfo{Name: "once"}, text("y")); err == nil || !strings.Contains(err.Error(), "already registered") {
		t.Fatalf("duplicate Register error = %v", err)
	}
	if err := r.Register(CommandInfo{Name: "nilfn"}, nil); err == nil {
		t.Fatal("nil run fn accepted")
	}
}

func TestUnregisterIdempotent(t *testing.T) {
	r := NewRegistry()
	if err := r.Register(CommandInfo{Name: "gone"}, text("x")); err != nil {
		t.Fatal(err)
	}
	r.Unregister("gone")
	r.Unregister("gone") // no panic
	if _, err := r.Run("gone", ""); err == nil {
		t.Fatal("unregistered command still runs")
	}
}

// The plugin's Apply provides the registry with the built-ins, /help
// aligns the usage column, and the UI-owned built-ins return their
// sentinels.
func TestPluginBuiltins(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	r, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	names := map[string]bool{}
	for _, in := range r.List() {
		names[in.Name] = true
	}
	for _, want := range []string{"help", "sessions", "clear", "collapse", "expand", "quit"} {
		if !names[want] {
			t.Fatalf("built-in /%s missing (have %v)", want, names)
		}
	}

	help, err := r.Run("help", "")
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"/help", "/sessions", "pick a session to resume"} {
		if !strings.Contains(help, want) {
			t.Fatalf("/help output missing %q:\n%s", want, help)
		}
	}

	for name, want := range map[string]UIAction{
		"clear": ActionClear, "collapse": ActionCollapse, "expand": ActionExpand, "quit": ActionQuit,
	} {
		_, err := r.Run(name, "")
		var a UIAction
		if !errors.As(err, &a) || a != want {
			t.Fatalf("/%s -> (%v), want UIAction %q", name, err, want)
		}
	}
}

// Built-ins list before skills, each group alphabetical; /help puts
// a "skills" heading over the skill rows and ellipsizes summaries at
// a word boundary.
func TestListBuiltinsBeforeSkills(t *testing.T) {
	r := NewRegistry()
	long := strings.Repeat("word ", 30)
	for _, in := range []CommandInfo{
		{Name: "zeta", Kind: "skill", Summary: long},
		{Name: "alpha", Kind: "skill", Summary: "a skill"},
		{Name: "quit", Kind: "builtin"},
		{Name: "help"},
	} {
		if err := r.Register(in, text("x")); err != nil {
			t.Fatal(err)
		}
	}
	var names []string
	for _, in := range r.List() {
		names = append(names, in.Name)
	}
	if got := strings.Join(names, " "); got != "help quit alpha zeta" {
		t.Fatalf("List order = %q, want builtins first then skills", got)
	}
	help := helpText(r)
	lines := strings.Split(help, "\n")
	if len(lines) != 5 || lines[2] != "skills" {
		t.Fatalf("/help should carry a skills heading before the skill rows:\n%s", help)
	}
	if strings.Contains(help, long) || !strings.Contains(help, "word…") {
		t.Fatalf("/help should ellipsize long summaries at a word boundary:\n%s", help)
	}
}

// Templates list after the built-ins and before the skills, under
// their own /help heading.
func TestListAndHelpGroupTemplates(t *testing.T) {
	r := NewRegistry()
	for _, in := range []CommandInfo{
		{Name: "zskill", Kind: "skill", Summary: "skill: z"},
		{Name: "review", Kind: "template", Summary: "template: Review a diff"},
		{Name: "quit", Kind: "builtin"},
		{Name: "greet", Kind: "template", Summary: "template: greet"},
	} {
		if err := r.Register(in, text("x")); err != nil {
			t.Fatal(err)
		}
	}
	var names []string
	for _, in := range r.List() {
		names = append(names, in.Name)
	}
	if got := strings.Join(names, " "); got != "quit greet review zskill" {
		t.Fatalf("List order = %q, want builtins, templates, skills", got)
	}
	lines := strings.Split(helpText(r), "\n")
	if len(lines) != 6 || lines[1] != "templates" || lines[4] != "skills" {
		t.Fatalf("/help should head the template and skill groups:\n%s", helpText(r))
	}
}

func TestEllipsize(t *testing.T) {
	for _, c := range []struct{ in, want string }{
		{"short", "short"},
		{"one two three four", "one two…"},
		{"averyveryverylongword", "averyvery…"},
	} {
		if got := Ellipsize(c.in, 10); got != c.want {
			t.Errorf("Ellipsize(%q, 10) = %q, want %q", c.in, got, c.want)
		}
	}
}

type stubUsage struct{ u llm.Usage }

func (s stubUsage) Usage() llm.Usage { return s.u }

func TestCostReportsUsage(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	r, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.Run("cost", ""); err == nil || !strings.Contains(err.Error(), "no usage") {
		t.Fatalf("/cost without a reporting llm = %v", err)
	}
	ctx.Provide("llm", stubUsage{})
	if out, _ := r.Run("cost", ""); !strings.Contains(out, "nothing used yet") {
		t.Fatalf("/cost on a fresh tally = %q", out)
	}
	ctx.Provide("llm", stubUsage{llm.Usage{InputTokens: 2000, OutputTokens: 100, Cost: 0.05, Priced: true}})
	out, err := r.Run("cost", "")
	if err != nil || !strings.Contains(out, "2.0k in · 100 out · $0.0500") {
		t.Fatalf("/cost = (%q, %v)", out, err)
	}
	if _, err := r.Run("keys", ""); !errors.Is(err, ActionKeys) {
		t.Fatalf("/keys should return the keys UIAction, got %v", err)
	}
}

// /sessions opens the picker; /sessions <id> is a resume action for
// that id (a trailing .jsonl is tolerated).
func TestSessionsOpensPickerOrResumesID(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	r, err := kernel.Get[*Registry](ctx, "commands")
	if err != nil {
		t.Fatal(err)
	}
	var a UIAction
	if _, err := r.Run("sessions", ""); !errors.As(err, &a) || a != ActionOpenPicker {
		t.Fatalf("/sessions = %v, want %q", err, ActionOpenPicker)
	}
	if _, err := r.Run("sessions", " 2026-09-01T00:00:00Z-1.jsonl "); !errors.As(err, &a) {
		t.Fatalf("/sessions <id> = %v, want a UIAction", err)
	}
	if id, ok := ResumeID(a); !ok || id != "2026-09-01T00:00:00Z-1" {
		t.Fatalf("ResumeID(%q) = %q, %v", a, id, ok)
	}
	if _, ok := ResumeID(ActionOpenPicker); ok {
		t.Fatal("open-picker is not a resume action")
	}
}
