// Package commands is the "commands" plugin: a registry of slash
// commands ("/help", "/quit", ...) behind the "commands" service. The
// registry only lists and runs commands — parsing the composer line,
// palette UI, and rendering output are the ui plugin's job, and a
// dispatched "/" line never reaches the LLM.
//
// A command's Run returns either text output or, for effects only the
// UI can perform (clear, collapse, expand, quit, open the session
// picker), a UIAction sentinel on the error channel; the UI detects it
// with errors.As. Every command yields output or a reason: an empty
// output with a nil error is echoed back as "/name" (the M27 rule).
package commands

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// CommandInfo describes one command for /help and the palette.
type CommandInfo struct {
	Name    string // without the leading "/"
	Usage   string // argument hint, "" for none
	Summary string // one line, shown dimmed in the palette
	Kind    string // "builtin" (default when ""), "user", "template", "skill"
}

// IsSkill reports whether the command is a skill ("/name" submits to
// the loop) — the UI ranks and styles those below the built-ins.
func (c CommandInfo) IsSkill() bool { return c.Kind == "skill" }

// IsTemplate reports whether the command is a prompt template ("/name
// args" submits the expanded file) — listed like a skill, below the
// built-ins, under its own /help heading.
func (c CommandInfo) IsTemplate() bool { return c.Kind == "template" }

// group orders List: built-ins and user commands, then templates,
// then skills.
func (c CommandInfo) group() int {
	switch {
	case c.IsSkill():
		return 2
	case c.IsTemplate():
		return 1
	}
	return 0
}

// groupHeading is the /help heading over each non-zero group.
var groupHeading = [...]string{"", "templates", "skills"}

// UIAction is the sentinel a UI-owned command returns on the error
// channel: Run gives ("", UIAction("...")) and the UI detects it with
// errors.As and performs the effect itself. It is not a failure.
type UIAction string

func (a UIAction) Error() string { return string(a) }

// The UI actions built-ins return.
const (
	ActionClear      UIAction = "clear"
	ActionCollapse   UIAction = "collapse"
	ActionExpand     UIAction = "expand"
	ActionQuit       UIAction = "quit"
	ActionOpenPicker UIAction = "open-picker"
	ActionKeys       UIAction = "keys" // /keys: the UI prints its live keymap
)

// submitPrefix marks a UIAction that submits text to the loop as if
// the user had typed it (skills: "/exa foo" becomes the input "/exa
// foo", which the skills seam then injects on).
const submitPrefix = "submit:"

// SubmitAction is the UIAction that submits text as user input.
func SubmitAction(text string) UIAction { return UIAction(submitPrefix + text) }

// SubmitText reports whether a is a submit action and the text to submit.
func SubmitText(a UIAction) (string, bool) {
	return strings.CutPrefix(string(a), submitPrefix)
}

// resumePrefix marks a UIAction that resumes one session by id
// ("/sessions <id>"): the UI swaps history through the session-choose
// seam and replays, as the picker does.
const resumePrefix = "resume:"

// ResumeAction is the UIAction that resumes the session id.
func ResumeAction(id string) UIAction { return UIAction(resumePrefix + id) }

// ResumeID reports whether a is a resume action and the session id.
func ResumeID(a UIAction) (string, bool) {
	return strings.CutPrefix(string(a), resumePrefix)
}

// modelPickerPrefix marks a UIAction that opens the model picker
// ("/model" with no args): the payload is the current "provider
// model" line followed by one choice per line, so the UI (or a
// headless run, which prints them) needs no llm knowledge of its own.
const modelPickerPrefix = "model-picker:"

// ModelPickerAction is the UIAction that opens the model picker over
// choices ("provider" or "provider model"), current marking the row in
// effect. target is "" for the agent's own model and "small" for the
// cheap one, so the picker knows which row a choice changes.
func ModelPickerAction(target, current string, choices []string) UIAction {
	return UIAction(modelPickerPrefix + target + "\t" + current + "\n" + strings.Join(choices, "\n"))
}

// ModelPickerChoices reports whether a is a model-picker action and
// its target, current line and choices.
func ModelPickerChoices(a UIAction) (target, current string, choices []string, ok bool) {
	body, ok := strings.CutPrefix(string(a), modelPickerPrefix)
	if !ok {
		return "", "", nil, false
	}
	lines := strings.Split(body, "\n")
	target, current, _ = strings.Cut(lines[0], "\t")
	return target, current, lines[1:], true
}

// Registry is the "commands" service: a concurrency-safe name ->
// command table. Other plugins register against the concrete type.
type Registry struct {
	mu   sync.Mutex
	cmds map[string]command
}

type command struct {
	info CommandInfo
	run  func(args string) (string, error)
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{cmds: map[string]command{}}
}

// Register adds a command. The name must be non-empty, without the
// leading "/" and without whitespace; a duplicate name is an error.
func (r *Registry) Register(info CommandInfo, run func(args string) (string, error)) error {
	if info.Name == "" || strings.HasPrefix(info.Name, "/") || strings.ContainsAny(info.Name, " \t\n") {
		return fmt.Errorf("commands: bad name %q (want non-empty, no leading /, no whitespace)", info.Name)
	}
	if run == nil {
		return fmt.Errorf("commands: /%s: nil run function", info.Name)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, dup := r.cmds[info.Name]; dup {
		return fmt.Errorf("commands: /%s already registered", info.Name)
	}
	r.cmds[info.Name] = command{info: info, run: run}
	return nil
}

// Unregister removes a command; unknown names are a no-op (unmount
// cleanup must be idempotent).
func (r *Registry) Unregister(name string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.cmds, name)
}

// List returns every command: built-ins (and user commands) first,
// then templates, then skills, each group sorted by name.
func (r *Registry) List() []CommandInfo {
	r.mu.Lock()
	infos := make([]CommandInfo, 0, len(r.cmds))
	for _, c := range r.cmds {
		infos = append(infos, c.info)
	}
	r.mu.Unlock()
	slices.SortFunc(infos, func(a, b CommandInfo) int {
		return cmp.Or(cmp.Compare(a.group(), b.group()), cmp.Compare(a.Name, b.Name))
	})
	return infos
}

// Run dispatches one command. An unknown name is the canonical
// "unknown command: /x (try /help)" error. A command's own error (or
// UIAction sentinel) passes through unchanged; empty output with a nil
// error comes back as "/name" so every command shows something.
func (r *Registry) Run(name, args string) (string, error) {
	r.mu.Lock()
	c, ok := r.cmds[name]
	r.mu.Unlock()
	if !ok {
		return "", fmt.Errorf("unknown command: /%s (try /help)", name)
	}
	out, err := c.run(args)
	if err != nil {
		return "", err
	}
	if out == "" {
		out = "/" + name
	}
	return out, nil
}

// registerBuiltins installs the stock commands. UI-owned effects
// return UIAction sentinels; /help produces text here.
func registerBuiltins(r *Registry, ctx *kernel.Context) error {
	builtins := []struct {
		info CommandInfo
		run  func(args string) (string, error)
	}{
		{CommandInfo{Name: "help", Usage: "", Summary: "list commands"}, func(string) (string, error) {
			return helpText(r), nil
		}},
		{CommandInfo{Name: "keys", Usage: "", Summary: "show the keybindings"}, uiAction(ActionKeys)},
		{CommandInfo{Name: "sessions", Usage: "[id]", Summary: "pick a session to resume"}, func(args string) (string, error) {
			if id := strings.TrimSpace(args); id != "" {
				return "", ResumeAction(strings.TrimSuffix(id, ".jsonl"))
			}
			return "", ActionOpenPicker
		}},
		{CommandInfo{Name: "cost", Usage: "", Summary: "tokens and cost this session"}, func(string) (string, error) {
			return costText(ctx)
		}},
		{CommandInfo{Name: "clear", Usage: "", Summary: "clear the visible transcript"}, uiAction(ActionClear)},
		{CommandInfo{Name: "collapse", Usage: "", Summary: "collapse all blocks"}, uiAction(ActionCollapse)},
		{CommandInfo{Name: "expand", Usage: "", Summary: "expand all blocks"}, uiAction(ActionExpand)},
		{CommandInfo{Name: "quit", Usage: "", Summary: "exit bough"}, uiAction(ActionQuit)},
	}
	for _, b := range builtins {
		b.info.Kind = "builtin"
		if err := r.Register(b.info, b.run); err != nil {
			return err
		}
	}
	if err := registerModel(r, ctx); err != nil {
		return err
	}
	return registerTree(r, ctx)
}

func uiAction(a UIAction) func(string) (string, error) {
	return func(string) (string, error) { return "", a }
}

// helpSummaryMax caps a /help summary (the UI wraps the block to the
// terminal, but a paragraph-long skill description is still noise).
const helpSummaryMax = 72

// helpText renders every command as "/name usage  summary" with the
// left column padded to one shared width; built-ins first, then a
// "templates" heading over the template rows and "skills" over the
// skill rows.
func helpText(r *Registry) string {
	infos := r.List()
	lefts := make([]string, len(infos))
	width := 0
	for i, in := range infos {
		lefts[i] = "/" + in.Name
		if in.Usage != "" {
			lefts[i] += " " + in.Usage
		}
		if len(lefts[i]) > width {
			width = len(lefts[i])
		}
	}
	var b strings.Builder
	group := 0
	for i, in := range infos {
		if g := in.group(); g != group {
			group = g
			b.WriteString(groupHeading[g] + "\n")
		}
		fmt.Fprintf(&b, "%-*s  %s\n", width, lefts[i], Ellipsize(in.Summary, helpSummaryMax))
	}
	return strings.TrimRight(b.String(), "\n")
}

// Ellipsize clips s to at most n runes, breaking at a word boundary
// where one exists and ending with "…" rather than a cut word.
func Ellipsize(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	if n <= 1 {
		return "…"
	}
	cut := n - 1
	if i := strings.LastIndexAny(string(r[:cut+1]), " \t"); i > 0 {
		if k := len([]rune(string(r[:cut+1])[:i])); k >= n/2 {
			cut = k
		}
	}
	return strings.TrimRight(string(r[:cut]), " \t") + "…"
}

// costText is /cost: the llm service's running tally, when it reports
// one.
func costText(ctx *kernel.Context) (string, error) {
	// The cost row's priced view first (it also says where the price
	// came from); the llm's own tally otherwise.
	rep, err := kernel.Get[llm.UsageReporter](ctx, "usage")
	if err != nil {
		if rep, err = kernel.Get[llm.UsageReporter](ctx, "llm"); err != nil {
			return "", fmt.Errorf("cost: the llm provider reports no usage")
		}
	}
	u := rep.Usage()
	if u.String() == "" {
		return "cost: nothing used yet this session", nil
	}
	out := "cost: " + u.String()
	if s, ok := rep.(interface{ Source() string }); ok {
		out += " — " + s.Source()
	} else if !u.Priced {
		out += " (this provider reports tokens, not price; mount the cost row)"
	}
	return out, nil
}

type plugin struct{}

func init() {
	kernel.Register("commands", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "commands" }
func (plugin) Inject() []string { return nil }

// Apply provides the "commands" registry with the built-ins installed.
// /model resolves the services it needs lazily at Run time, so this
// row has no mount-order dependency.
func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	r := NewRegistry()
	if err := registerBuiltins(r, ctx); err != nil {
		return err
	}
	ctx.Provide("commands", r)
	return nil
}
