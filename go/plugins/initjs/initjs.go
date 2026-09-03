// Package initjs is the "init-js" plugin: Maki-style user configuration
// in JavaScript. At Apply it executes ~/.bough/init.js then
// ./.bough/init.js (both optional) in the shared codemode VM with a
// global `bough` API, and Provides the services those files configure:
// "theme", "keymap", "llm" (a JS provider), "cognition", "projection".
// Unknown setup keys, bad style strings, and JS errors fail Apply loud.
package initjs

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/dop251/goja"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

const initTimeout = 30 * time.Second

// vmHost is the slice of the codemode service this plugin needs.
type vmHost interface {
	WithVM(fn func(vm *goja.Runtime, tools *goja.Object) error) error
	Call(fn goja.Callable, args ...any) (any, error)
}

// themeTokens and keymapActions are the closed vocabularies of the
// "theme" and "keymap" service contracts; anything else is a typo.
var themeTokens = set("user", "assistant", "code", "result", "error", "accent", "dim", "border", "status")
var keymapActions = set("quit", "scroll_up", "scroll_down", "page_up", "page_down", "history_inspect", "collapse_toggle", "clear_input", "todo_toggle", "follow_up")

func set(keys ...string) map[string]bool {
	m := make(map[string]bool, len(keys))
	for _, k := range keys {
		m[k] = true
	}
	return m
}

// state accumulates what the init files register. All mutation happens
// on the VM goroutine under the codemode mutex.
type state struct {
	theme     map[string]string
	keymap    map[string]string
	providers map[string]goja.Callable
	defProv   string
	sysAppend string
	cogFn     goja.Callable
	projFn    goja.Callable
	toolNames []string
	cmdNames  []string
	setupUsed bool // reset per init file: setup is callable once per file
	sealed    bool // set after init files run: config surface closes
}

// install defines the global `bough` object. Callbacks panic with a JS
// value on misuse, which goja throws as a JS exception — so a typo
// stops the init file and surfaces as an Apply error. cm is used to
// call JS command fns later, under the VM mutex; reg is the "commands"
// registry bough.command registers into.
func install(vm *goja.Runtime, tools *goja.Object, st *state, cm vmHost, reg *commands.Registry) {
	throw := func(format string, a ...any) {
		panic(vm.ToValue(fmt.Sprintf(format, a...)))
	}
	oneFn := func(what string) func(goja.FunctionCall) goja.Callable {
		return func(call goja.FunctionCall) goja.Callable {
			if st.sealed {
				throw("%s: only available during init", what)
			}
			if len(call.Arguments) != 1 {
				throw("%s: want one function argument", what)
			}
			fn, ok := goja.AssertFunction(call.Argument(0))
			if !ok {
				throw("%s: argument is not a function", what)
			}
			return fn
		}
	}

	b := vm.NewObject()
	b.Set("setup", func(call goja.FunctionCall) goja.Value {
		if st.sealed {
			throw("bough.setup: only available during init")
		}
		if st.setupUsed {
			throw("bough.setup: called twice in one init file")
		}
		st.setupUsed = true
		if len(call.Arguments) != 1 {
			throw("bough.setup: want one object argument")
		}
		m, ok := call.Argument(0).Export().(map[string]any)
		if !ok {
			throw("bough.setup: argument is not an object")
		}
		if err := applySetup(st, m); err != nil {
			throw("%s", err)
		}
		return goja.Undefined()
	})
	b.Set("tool", func(call goja.FunctionCall) goja.Value {
		// Deliberately allowed after init too: live tool registration
		// from model code is the Cordis-flavored extension path.
		if len(call.Arguments) != 2 {
			throw("bough.tool: want (name, fn)")
		}
		name, ok := call.Argument(0).Export().(string)
		if !ok || name == "" {
			throw("bough.tool: name is not a non-empty string")
		}
		if _, ok := goja.AssertFunction(call.Argument(1)); !ok {
			throw("bough.tool: fn is not a function")
		}
		tools.Set(name, call.Argument(1))
		st.toolNames = append(st.toolNames, name)
		return goja.Undefined()
	})
	b.Set("command", func(call goja.FunctionCall) goja.Value {
		// Like bough.tool, deliberately allowed after init too: live
		// command registration from model code is the extension path.
		if len(call.Arguments) != 4 {
			throw("bough.command: want (name, usage, summary, fn)")
		}
		name, ok := call.Argument(0).Export().(string)
		if !ok || name == "" {
			throw("bough.command: name is not a non-empty string")
		}
		usage, ok := call.Argument(1).Export().(string)
		if !ok {
			throw("bough.command: usage is not a string")
		}
		summary, ok := call.Argument(2).Export().(string)
		if !ok {
			throw("bough.command: summary is not a string")
		}
		fn, ok := goja.AssertFunction(call.Argument(3))
		if !ok {
			throw("bough.command: fn is not a function")
		}
		info := commands.CommandInfo{Name: name, Usage: usage, Summary: summary}
		err := reg.Register(info, func(args string) (string, error) {
			v, err := cm.Call(fn, args)
			if err != nil {
				return "", fmt.Errorf("init-js: command /%s: %w", name, err)
			}
			if v == nil {
				return "", nil
			}
			s, ok := v.(string)
			if !ok {
				return "", fmt.Errorf("init-js: command /%s returned %T, not string", name, v)
			}
			return s, nil
		})
		if err != nil {
			throw("bough.command: %s", err)
		}
		st.cmdNames = append(st.cmdNames, name)
		return goja.Undefined()
	})
	b.Set("provider", func(call goja.FunctionCall) goja.Value {
		if st.sealed {
			throw("bough.provider: only available during init")
		}
		if len(call.Arguments) != 2 {
			throw("bough.provider: want (name, fn)")
		}
		name, ok := call.Argument(0).Export().(string)
		if !ok || name == "" {
			throw("bough.provider: name is not a non-empty string")
		}
		fn, ok := goja.AssertFunction(call.Argument(1))
		if !ok {
			throw("bough.provider: fn is not a function")
		}
		st.providers[name] = fn
		return goja.Undefined()
	})
	takeCog := oneFn("bough.cognition")
	b.Set("cognition", func(call goja.FunctionCall) goja.Value {
		st.cogFn = takeCog(call)
		return goja.Undefined()
	})
	takeProj := oneFn("bough.project")
	b.Set("project", func(call goja.FunctionCall) goja.Value {
		st.projFn = takeProj(call)
		return goja.Undefined()
	})
	vm.Set("bough", b)
}

// applySetup validates and merges one bough.setup({...}) object.
// Unknown keys at any level are errors naming the key.
func applySetup(st *state, m map[string]any) error {
	for k, v := range m {
		switch k {
		case "ui":
			ui, ok := v.(map[string]any)
			if !ok {
				return fmt.Errorf("bough.setup: ui is not an object")
			}
			for uk, uv := range ui {
				switch uk {
				case "theme":
					if err := mergeStrMap(st.theme, uv, "ui.theme", themeTokens, validStyle); err != nil {
						return err
					}
				case "keymap":
					if err := mergeStrMap(st.keymap, uv, "ui.keymap", keymapActions, validKeyName); err != nil {
						return err
					}
				default:
					return fmt.Errorf("bough.setup: unknown key ui.%s", uk)
				}
			}
		case "provider":
			p, ok := v.(map[string]any)
			if !ok {
				return fmt.Errorf("bough.setup: provider is not an object")
			}
			for pk, pv := range p {
				if pk != "default" {
					return fmt.Errorf("bough.setup: unknown key provider.%s", pk)
				}
				s, ok := pv.(string)
				if !ok {
					return fmt.Errorf("bough.setup: provider.default is not a string")
				}
				st.defProv = s
			}
		case "system":
			sy, ok := v.(map[string]any)
			if !ok {
				return fmt.Errorf("bough.setup: system is not an object")
			}
			for sk, sv := range sy {
				if sk != "append" {
					return fmt.Errorf("bough.setup: unknown key system.%s", sk)
				}
				s, ok := sv.(string)
				if !ok {
					return fmt.Errorf("bough.setup: system.append is not a string")
				}
				st.sysAppend = s
			}
		default:
			return fmt.Errorf("bough.setup: unknown key %q", k)
		}
	}
	return nil
}

// mergeStrMap merges a JS object of string->string into dst, rejecting
// keys outside allowed and values failing check. Later merges (the
// project file) overwrite earlier ones (the global file).
func mergeStrMap(dst map[string]string, v any, where string, allowed map[string]bool, check func(string) error) error {
	m, ok := v.(map[string]any)
	if !ok {
		return fmt.Errorf("bough.setup: %s is not an object", where)
	}
	for k, val := range m {
		if !allowed[k] {
			return fmt.Errorf("bough.setup: unknown %s key %q", where, k)
		}
		s, ok := val.(string)
		if !ok {
			return fmt.Errorf("bough.setup: %s.%s is not a string", where, k)
		}
		if err := check(s); err != nil {
			return fmt.Errorf("bough.setup: %s.%s: %v", where, k, err)
		}
		dst[k] = s
	}
	return nil
}

// Style grammar: "fg[:bg][:bold|italic|faint]" — colors are #rrggbb hex
// or ANSI-256 numbers; attributes follow colors.
var hexColor = regexp.MustCompile(`^#[0-9a-fA-F]{6}$`)
var styleAttrs = set("bold", "italic", "faint")

func validColor(s string) bool {
	if hexColor.MatchString(s) {
		return true
	}
	n, err := strconv.Atoi(s)
	return err == nil && n >= 0 && n <= 255
}

func validStyle(s string) error {
	parts := strings.Split(s, ":")
	if len(parts) > 3 {
		return fmt.Errorf("style %q has more than 3 segments (want fg[:bg][:attr])", s)
	}
	if !validColor(parts[0]) {
		return fmt.Errorf("bad fg color %q (want #rrggbb or 0-255)", parts[0])
	}
	colors, attrs := 1, 0
	for _, p := range parts[1:] {
		switch {
		case styleAttrs[p]:
			attrs++
		case validColor(p):
			colors++
			if attrs > 0 {
				return fmt.Errorf("style %q: bg color after attribute (want fg[:bg][:attr])", s)
			}
			if colors > 2 {
				return fmt.Errorf("style %q: more than two colors", s)
			}
		default:
			return fmt.Errorf("style %q: bad segment %q (want a color or bold|italic|faint)", s, p)
		}
	}
	return nil
}

func validKeyName(s string) error {
	if s == "" {
		return fmt.Errorf("empty key")
	}
	return nil
}

// jsProvider backs the "llm" service with a JS fn(system, messages) ->
// string, invoked under the VM mutex with the codemode call timeout.
// Messages cross as [{role, content}].
type jsProvider struct {
	cm   vmHost
	name string
	fn   goja.Callable
}

func (p *jsProvider) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	msgs := make([]map[string]any, len(messages))
	for i, m := range messages {
		msgs[i] = map[string]any{"role": m.Role, "content": m.Content}
	}
	v, err := p.cm.Call(p.fn, system, msgs)
	if err != nil {
		return "", fmt.Errorf("init-js: provider %q: %w", p.name, err)
	}
	s, ok := v.(string)
	if !ok {
		return "", fmt.Errorf("init-js: provider %q returned %T, not string", p.name, v)
	}
	return s, nil
}

// jsCognition backs "cognition" with fn(baseSystem) -> string. A JS
// error is logged loud and the base prompt is used unchanged (the
// contract has no error channel).
type jsCognition struct {
	cm vmHost
	fn goja.Callable
}

func (c *jsCognition) System(base string) string {
	v, err := c.cm.Call(c.fn, base)
	if err != nil {
		fmt.Fprintf(os.Stderr, "init-js: cognition: %v (using base prompt)\n", err)
		return base
	}
	s, ok := v.(string)
	if !ok {
		fmt.Fprintf(os.Stderr, "init-js: cognition returned %T, not string (using base prompt)\n", v)
		return base
	}
	return s
}

// appendCognition is the setup.system.append shorthand.
type appendCognition struct{ extra string }

func (a appendCognition) System(base string) string { return base + "\n\n" + a.extra }

// jsProjection backs "projection" with fn(entries) -> [{role, content}].
// Entries cross as plain objects {seq, at (RFC3339Nano), kind, data}.
// A JS error or bad shape logs loud and falls back to the built-in
// projection so the loop keeps working.
type jsProjection struct {
	cm vmHost
	fn goja.Callable
}

func (p *jsProjection) Project(entries []history.Entry) []llm.Message {
	plain := make([]map[string]any, len(entries))
	for i, e := range entries {
		plain[i] = map[string]any{
			"seq":  e.Seq,
			"at":   e.At.Format(time.RFC3339Nano),
			"kind": e.Kind,
			"data": e.Data,
		}
	}
	v, err := p.cm.Call(p.fn, plain)
	if err != nil {
		fmt.Fprintf(os.Stderr, "init-js: projection: %v (using default projection)\n", err)
		return loop.DefaultProject(entries)
	}
	arr, ok := v.([]any)
	if !ok {
		fmt.Fprintf(os.Stderr, "init-js: projection returned %T, not an array (using default projection)\n", v)
		return loop.DefaultProject(entries)
	}
	msgs := make([]llm.Message, 0, len(arr))
	for i, item := range arr {
		m, ok := item.(map[string]any)
		if !ok {
			fmt.Fprintf(os.Stderr, "init-js: projection[%d] is %T, not an object (using default projection)\n", i, item)
			return loop.DefaultProject(entries)
		}
		role, rok := m["role"].(string)
		content, cok := m["content"].(string)
		if !rok || !cok {
			fmt.Fprintf(os.Stderr, "init-js: projection[%d] missing string role/content (using default projection)\n", i)
			return loop.DefaultProject(entries)
		}
		msgs = append(msgs, llm.Message{Role: role, Content: content})
	}
	return msgs
}

type plugin struct{}

func init() {
	kernel.Register("init-js", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "init-js" }
func (plugin) Inject() []string { return []string{"codemode", "commands"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	cm, err := kernel.Get[vmHost](ctx, "codemode")
	if err != nil {
		return err
	}
	reg, err := kernel.Get[*commands.Registry](ctx, "commands")
	if err != nil {
		return err
	}
	st := &state{
		theme:     map[string]string{},
		keymap:    map[string]string{},
		providers: map[string]goja.Callable{},
	}

	var files []string
	if home, err := os.UserHomeDir(); err == nil {
		files = append(files, filepath.Join(home, ".bough", "init.js"))
	}
	files = append(files, filepath.Join(".bough", "init.js"))

	err = cm.WithVM(func(vm *goja.Runtime, tools *goja.Object) error {
		install(vm, tools, st, cm, reg)
		for _, path := range files {
			body, err := os.ReadFile(path)
			if err != nil {
				if os.IsNotExist(err) {
					continue
				}
				return fmt.Errorf("init-js: %s: %w", path, err)
			}
			st.setupUsed = false
			if err := runFile(vm, path, string(body)); err != nil {
				return err
			}
		}
		st.sealed = true
		return nil
	})
	if err != nil {
		return err
	}

	// Unmount: drop the global, JS-registered tools, and JS-registered
	// commands so a remount (or removal of this row) leaves the shared
	// VM and the commands registry clean.
	ctx.Effect(func() {
		_ = cm.WithVM(func(vm *goja.Runtime, tools *goja.Object) error {
			_ = vm.GlobalObject().Delete("bough")
			for _, n := range st.toolNames {
				_ = tools.Delete(n)
			}
			return nil
		})
		for _, n := range st.cmdNames {
			reg.Unregister(n)
		}
	})

	if len(st.theme) > 0 {
		ctx.Provide("theme", st.theme)
	}
	if len(st.keymap) > 0 {
		ctx.Provide("keymap", st.keymap)
	}
	if st.defProv != "" {
		fn, ok := st.providers[st.defProv]
		if !ok {
			return fmt.Errorf("init-js: setup.provider.default %q: no bough.provider registered under that name", st.defProv)
		}
		// The kernel's last-write-wins warning over the yml llm row is
		// the designed shadowing path.
		ctx.Provide("llm", &jsProvider{cm: cm, name: st.defProv, fn: fn})
	}
	switch {
	case st.cogFn != nil:
		if st.sysAppend != "" {
			fmt.Fprintln(os.Stderr, "init-js: WARNING: both bough.cognition and setup.system.append set; cognition wins, append ignored")
		}
		ctx.Provide("cognition", &jsCognition{cm: cm, fn: st.cogFn})
	case st.sysAppend != "":
		ctx.Provide("cognition", appendCognition{st.sysAppend})
	}
	if st.projFn != nil {
		ctx.Provide("projection", &jsProjection{cm: cm, fn: st.projFn})
	}
	return nil
}

// runFile executes one init file under its own interrupt timeout.
func runFile(vm *goja.Runtime, path, body string) error {
	timer := time.AfterFunc(initTimeout, func() {
		vm.Interrupt("init-js: timeout after " + initTimeout.String())
	})
	defer func() {
		timer.Stop()
		vm.ClearInterrupt()
	}()
	if _, err := vm.RunScript(path, body); err != nil {
		return fmt.Errorf("init-js: %s: %w", path, err)
	}
	return nil
}
