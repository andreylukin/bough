// Package codemode provides the "codemode" service: a goja JS runtime
// with a global "tools" object that plugins register Go functions into.
package codemode

import (
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/dop251/goja"

	"github.com/andreylukin/bough/kernel"
)

// CodeMode is one persistent goja VM. Not safe for concurrent Run;
// a mutex serializes all access.
type CodeMode struct {
	mu      sync.Mutex
	vm      *goja.Runtime
	tools   *goja.Object
	timeout time.Duration
	out     strings.Builder
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
	return cm
}

// RegisterTool binds fn under tools.<name>. Tool fns have shape
// func(args...) (string, error); goja converts a non-nil trailing
// error into a JS exception.
func (cm *CodeMode) RegisterTool(name string, fn any) {
	cm.mu.Lock()
	defer cm.mu.Unlock()
	cm.tools.Set(name, fn)
}

// Run executes code with an interrupt after the timeout. Returns any
// console output plus the final expression value if non-undefined.
func (cm *CodeMode) Run(code string) (string, error) {
	cm.mu.Lock()
	defer cm.mu.Unlock()
	cm.out.Reset()

	timer := time.AfterFunc(cm.timeout, func() {
		cm.vm.Interrupt("codemode: timeout after " + cm.timeout.String())
	})
	v, err := cm.vm.RunString(code)
	timer.Stop()
	cm.vm.ClearInterrupt()

	out := cm.out.String()
	if err != nil {
		return out, err
	}
	if v != nil && !goja.IsUndefined(v) {
		out += v.String()
	}
	return out, nil
}

// RunHook runs fileBody as the body of function(event){...} in the same
// VM (same mutex, same globals and tools.*) and calls it with event.
// A returned object comes back as map[string]any; no return (undefined
// or null) is nil, nil; a non-object return or JS exception is an error.
func (cm *CodeMode) RunHook(fileBody string, event map[string]any) (map[string]any, error) {
	cm.mu.Lock()
	defer cm.mu.Unlock()

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
