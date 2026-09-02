// Package codemode provides the "codemode" service: a goja JS runtime
// with a global "tools" object that plugins register Go functions into.
package codemode

import (
	"context"
	"fmt"
	"regexp"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/dop251/goja"

	"github.com/andreylukin/bough/kernel"
)

// CodeMode is one persistent goja VM. Not safe for concurrent Run;
// a mutex serializes all access. The mutex is re-entrant for the
// goroutine that holds it (see lock): a Go tool called from inside Run
// may re-enter the VM (e.g. workers' tools.spawn running a subagent's
// code blocks), which is safe because the VM sits idle waiting for the
// native call to return. Cross-goroutine callers still serialize.
type CodeMode struct {
	mu      sync.Mutex
	owner   atomic.Int64 // goroutine id holding mu; 0 when free
	vm      *goja.Runtime
	tools   *goja.Object
	timeout time.Duration
	out     strings.Builder
	timer   *time.Timer     // innermost Run's interrupt timer; see Pause
	scoped  goja.Callable   // (src) => eval(src): per-block function scope
	runCtx  context.Context // the innermost RunCtx's context; Background between runs
}

// nativeFrame is the Go-frame tail goja appends to a GoError message
// ("... at github.com/andreylukin/bough/plugins/tools.view (native)");
// noise to the model and the user, stripped by cleanErr.
var nativeFrame = regexp.MustCompile(` at github\.com/andreylukin/bough/\S+ \(native\)`)

// cleanErr strips native Go frames from a JS error message.
func cleanErr(err error) error {
	msg := err.Error()
	if c := nativeFrame.ReplaceAllString(msg, ""); c != msg {
		return fmt.Errorf("%s", c)
	}
	return err
}

// gid returns the current goroutine id (parsed from runtime.Stack),
// used only to detect same-goroutine re-entry in lock.
func gid() int64 {
	var buf [64]byte
	n := runtime.Stack(buf[:], false)
	s := strings.TrimPrefix(string(buf[:n]), "goroutine ")
	if i := strings.IndexByte(s, ' '); i > 0 {
		if id, err := strconv.ParseInt(s[:i], 10, 64); err == nil {
			return id
		}
	}
	return -1
}

// lock takes the VM mutex, or reports nested=true when this goroutine
// already holds it (a tool fn re-entering the VM mid-Run).
func (cm *CodeMode) lock() (nested bool) {
	g := gid()
	if cm.owner.Load() == g {
		return true
	}
	cm.mu.Lock()
	cm.owner.Store(g)
	return false
}

func (cm *CodeMode) unlock(nested bool) {
	if nested {
		return
	}
	cm.owner.Store(0)
	cm.mu.Unlock()
}

// New builds a VM with a "tools" object and a console.log that
// captures into the output buffer.
func New(timeout time.Duration) *CodeMode {
	cm := &CodeMode{vm: goja.New(), timeout: timeout}
	cm.tools = cm.vm.NewObject()
	cm.vm.Set("tools", cm.tools)

	console := cm.vm.NewObject()
	console.Set("log", func(call goja.FunctionCall) goja.Value {
		parts := make([]string, 0, len(call.Arguments))
		for _, a := range call.Arguments {
			parts = append(parts, a.String())
		}
		cm.out.WriteString(strings.Join(parts, " ") + "\n")
		return goja.Undefined()
	})
	cm.vm.Set("console", console)

	// Each Run evaluates its block via direct eval inside a fresh
	// function call, so const/let/var/function declarations are scoped
	// to that block (a later block may reuse the names) while globals
	// (tools, console, anything assigned without a declaration) stay
	// shared. eval keeps the block's completion value.
	v, err := cm.vm.RunString("(function(__src){ return eval(__src) })")
	if err != nil {
		panic("codemode: scoped runner: " + err.Error())
	}
	cm.scoped, _ = goja.AssertFunction(v)
	return cm
}

// RegisterTool binds fn under tools.<name>. Tool fns have shape
// func(args...) (string, error); goja converts a non-nil trailing
// error into a JS exception.
func (cm *CodeMode) RegisterTool(name string, fn any) {
	nested := cm.lock()
	defer cm.unlock(nested)
	cm.tools.Set(name, fn)
}

// Run executes code with an interrupt after the timeout. Returns any
// console output plus the final expression value if non-undefined.
// Declarations are scoped to the block (see New); errors have their
// native Go frames stripped (cleanErr).
// A nested Run (a subagent's code block executing while the parent's
// block waits on its tool call) saves and restores the parent's console
// output around the child's; the child's ClearInterrupt may drop a
// concurrently-fired outer timeout (accepted v0), and the outer timer
// keeps ticking, so a long child run can still trip the parent's
// timeout.
func (cm *CodeMode) Run(code string) (string, error) {
	return cm.RunCtx(context.Background(), code)
}

// RunContext is the context of the run in progress: a host call that
// blocks (tools.bash) derives its own from it, so cancelling the turn
// kills the command instead of waiting for it. Background when idle.
func (cm *CodeMode) RunContext() context.Context {
	nested := cm.lock()
	defer cm.unlock(nested)
	if cm.runCtx == nil {
		return context.Background()
	}
	return cm.runCtx
}

// RunCtx is Run under ctx: the VM is interrupted when ctx is done and
// host calls see it through RunContext.
func (cm *CodeMode) RunCtx(ctx context.Context, code string) (string, error) {
	nested := cm.lock()
	defer cm.unlock(nested)
	prevCtx := cm.runCtx
	cm.runCtx = ctx
	defer func() { cm.runCtx = prevCtx }()
	saved := ""
	if nested {
		saved = cm.out.String()
	}
	cm.out.Reset()

	timer := time.AfterFunc(cm.timeout, func() {
		cm.vm.Interrupt("codemode: timeout after " + cm.timeout.String())
	})
	prevTimer := cm.timer // nested Run: restore the parent's timer after
	cm.timer = timer
	cm.vm.ClearInterrupt() // a cancel that landed between runs must not abort this one
	v, err := cm.scoped(goja.Undefined(), cm.vm.ToValue(code))
	cm.timer = prevTimer
	timer.Stop()
	cm.vm.ClearInterrupt()

	out := cm.out.String()
	if nested {
		cm.out.Reset()
		cm.out.WriteString(saved)
	}
	if err != nil {
		return out, cleanErr(err)
	}
	if v != nil && !goja.IsUndefined(v) {
		out += v.String()
	}
	return out, nil
}

// Pause stops the current Run's interrupt timer so a tool may block on
// external input (e.g. tools.ask waiting on the user) longer than the
// script timeout; the returned resume re-arms a fresh full timeout.
// Only meaningful from inside a running tool (the VM goroutine, under
// the Run lock); outside a Run it is a no-op.
func (cm *CodeMode) Pause() func() {
	t := cm.timer
	if t == nil {
		return func() {}
	}
	t.Stop()
	return func() { t.Reset(cm.timeout) }
}

// Interrupt aborts the running script (the loop's turn cancel); the
// next Run clears it, so an interrupt landing between runs is inert.
func (cm *CodeMode) Interrupt() {
	cm.vm.Interrupt("codemode: cancelled")
}

// RunHook runs fileBody as the body of function(event){...} in the same
// VM (same mutex, same globals and tools.*) and calls it with event.
// A returned object comes back as map[string]any; no return (undefined
// or null) is nil, nil; a non-object return or JS exception is an error.
func (cm *CodeMode) RunHook(fileBody string, event map[string]any) (map[string]any, error) {
	nested := cm.lock()
	defer cm.unlock(nested)

	v, err := cm.vm.RunString("(function(event){\n" + fileBody + "\n})")
	if err != nil {
		return nil, err
	}
	fn, ok := goja.AssertFunction(v)
	if !ok {
		return nil, fmt.Errorf("codemode: hook did not compile to a function")
	}

	timer := time.AfterFunc(cm.timeout, func() {
		cm.vm.Interrupt("codemode: hook timeout after " + cm.timeout.String())
	})
	ret, err := fn(goja.Undefined(), cm.vm.ToValue(event))
	timer.Stop()
	cm.vm.ClearInterrupt()
	if err != nil {
		return nil, err
	}
	if ret == nil || goja.IsUndefined(ret) || goja.IsNull(ret) {
		return nil, nil
	}
	m, ok := ret.Export().(map[string]any)
	if !ok {
		return nil, fmt.Errorf("codemode: hook returned %s, not an object", ret.ExportType())
	}
	return m, nil
}

// WithVM runs fn with the VM and the shared tools object while holding
// the VM mutex. For extension plugins (init-js) that define globals or
// run whole scripts; fn must not retain the runtime past the call.
func (cm *CodeMode) WithVM(fn func(vm *goja.Runtime, tools *goja.Object) error) error {
	nested := cm.lock()
	defer cm.unlock(nested)
	return fn(cm.vm, cm.tools)
}

// Call invokes a stored JS function with args (converted by goja) under
// the VM mutex and the standard interrupt timeout. The result is
// Exported to Go inside the lock; a JS exception or interrupt is an
// error, never a panic.
func (cm *CodeMode) Call(fn goja.Callable, args ...any) (out any, err error) {
	nested := cm.lock()
	defer cm.unlock(nested)
	defer func() {
		if r := recover(); r != nil {
			out, err = nil, fmt.Errorf("codemode: call panic: %v", r)
		}
	}()
	gargs := make([]goja.Value, len(args))
	for i, a := range args {
		gargs[i] = cm.vm.ToValue(a)
	}
	timer := time.AfterFunc(cm.timeout, func() {
		cm.vm.Interrupt("codemode: call timeout after " + cm.timeout.String())
	})
	v, err := fn(goja.Undefined(), gargs...)
	timer.Stop()
	cm.vm.ClearInterrupt()
	if err != nil {
		return nil, err
	}
	if v == nil {
		return nil, nil
	}
	return v.Export(), nil
}

type plugin struct{}

func init() {
	kernel.Register("codemode", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "codemode" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	ctx.Provide("codemode", New(30*time.Second))
	return nil
}
