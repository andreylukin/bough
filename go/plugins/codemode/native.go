package codemode

import (
	"context"
	"encoding/json"
	"strings"

	"github.com/andreylukin/bough/internal/agenttools"
)

// RunJSName is the native tool that runs a code block. The engine offers
// it only when its row says `tools: both`, and projects its calls as the
// loop's code/result pair.
const RunJSName = "run_js"

// nativeRunJS is a code block as a native tool: the session's own VM,
// with tools.* as the loop binds them. Parallel calls queue on the VM's
// mutex, since each arrives on its own goroutine; cancelling the call
// interrupts the block, as Esc interrupts the loop's.
func (cm *CodeMode) nativeRunJS() agenttools.Tool {
	return agenttools.Tool{
		Name: RunJSName,
		Description: "Run a JavaScript program in bough's code-mode VM, where every tools.* function (tools.bash, tools.view, …) is an ordinary synchronous call. " +
			"What it prints with console.log, and its last expression, come back. Use it to chain several steps whose intermediate output you do not need to read. No async/await, no Node APIs; globals persist between runs.",
		Schema: agenttools.Object([]string{"code"}, map[string]any{
			"code": agenttools.Prop("string", "the program"),
		}),
		Detail: func(args json.RawMessage) string {
			var a struct{ Code string }
			_ = json.Unmarshal(args, &a)
			line, _, _ := strings.Cut(strings.TrimSpace(a.Code), "\n")
			return line
		},
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			var a struct {
				Code string `json:"code"`
			}
			if err := agenttools.Decode(RunJSName, c.Args, &a); err != nil {
				return agenttools.Result{}, err
			}
			out, err := cm.RunCtx(ctx, a.Code)
			if err != nil {
				return agenttools.Result{Text: out, Error: err.Error()}, nil
			}
			return agenttools.Result{Text: out}, nil
		},
	}
}
