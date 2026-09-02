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
	"fmt"
	"sort"
	"strings"
	"sync"

	"github.com/andreylukin/bough/kernel"
)

// CommandInfo describes one command for /help and the palette.
type CommandInfo struct {
	Name    string // without the leading "/"
	Usage   string // argument hint, "" for none
	Summary string // one line, shown dimmed in the palette
}

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

// List returns every command, sorted by name.
func (r *Registry) List() []CommandInfo {
	r.mu.Lock()
	infos := make([]CommandInfo, 0, len(r.cmds))
	for _, c := range r.cmds {
		infos = append(infos, c.info)
	}
	r.mu.Unlock()
	sort.Slice(infos, func(i, j int) bool { return infos[i].Name < infos[j].Name })
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
		{CommandInfo{Name: "sessions", Usage: "[id]", Summary: "pick a session to resume"}, func(args string) (string, error) {
			if id := strings.TrimSpace(args); id != "" {
				return "", ResumeAction(strings.TrimSuffix(id, ".jsonl"))
			}
			return "", ActionOpenPicker
		}},
		{CommandInfo{Name: "clear", Usage: "", Summary: "clear the visible transcript"}, uiAction(ActionClear)},
		{CommandInfo{Name: "collapse", Usage: "", Summary: "collapse all blocks"}, uiAction(ActionCollapse)},
		{CommandInfo{Name: "expand", Usage: "", Summary: "expand all blocks"}, uiAction(ActionExpand)},
		{CommandInfo{Name: "quit", Usage: "", Summary: "exit bough"}, uiAction(ActionQuit)},
	}
	for _, b := range builtins {
		if err := r.Register(b.info, b.run); err != nil {
			return err
		}
	}
	return registerModel(r, ctx)
}

func uiAction(a UIAction) func(string) (string, error) {
	return func(string) (string, error) { return "", a }
}

// helpText renders every command as "/name usage  summary" with the
// left column padded to one shared width.
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
	for i, in := range infos {
		fmt.Fprintf(&b, "%-*s  %s\n", width, lefts[i], in.Summary)
	}
	return strings.TrimRight(b.String(), "\n")
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
