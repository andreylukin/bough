package commands

import (
	"errors"
	"sort"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
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
