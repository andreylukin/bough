package workers

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

// nativeSubSystemPrompt is SubSystemPrompt for a child that calls tools
// natively: it has no code blocks and no stop fence, and its final
// reply is the report.
const nativeSubSystemPrompt = `You are a bough subagent spawned for ONE task. Complete exactly that task with your tools (you cannot spawn, and you cannot ask the user), then end with your REPORT as your final reply — that is what hands your work back. The report is all the parent agent sees, so make it self-contained and short (under 30 lines):

Status: ok | failed
Findings: what you established, as bullets (facts, numbers, paths)
Files: paths you changed, or "none"
Open: questions or blockers for the parent, or "none"`

// spawner is the "engine" service's child runner (docs/unreal-engine.md
// §5.8), declared structurally so this row never imports the engine.
type spawner interface {
	Spawn(ctx context.Context, task, worker, system string, maxSteps int) (reply, status string, steps int, err error)
}

// nativeTools are spawn, agent and stop_agent for an engine that calls
// tools natively. A foreground spawn is a child coordinator the engine
// runs (its Spawn); parallel spawn calls are what spawnAll was, so
// spawnAll has no native form.
func (w *Workers) nativeTools() []agenttools.Tool {
	idArg := agenttools.Object([]string{"id"}, map[string]any{"id": agenttools.Prop("string", "the agent's session id")})
	idDetail := func(args json.RawMessage) string {
		var a struct{ ID string }
		_ = json.Unmarshal(args, &a)
		return a.ID
	}
	return []agenttools.Tool{
		{
			Name: "spawn",
			Description: "Delegate ONE self-contained task to a subagent with your tools and a fresh context, and get its report back. " +
				"Call spawn several times at once to fan out: the children run in parallel. It cannot see this conversation, so the task must say what to find, where, and what to report. " +
				"background: true starts a separate background agent session instead (project: run it in that project's container) and returns its id at once; you are told when it finishes.",
			Schema: agenttools.Object([]string{"task"}, map[string]any{
				"task":       agenttools.Prop("string", "the brief"),
				"background": agenttools.Prop("boolean", "start a separate background agent session"),
				"project":    agenttools.Prop("string", "background only: the project to run it in"),
				"model":      agenttools.Prop("string", "background only: plugin/model to run it on; default this session's"),
			}),
			Detail: func(args json.RawMessage) string {
				var a struct{ Task string }
				_ = json.Unmarshal(args, &a)
				return oneLine(a.Task, 80)
			},
			Call: w.nativeSpawn,
		},
		{
			Name:        "agent",
			Description: "A background agent you started: its status, title, reply and project.",
			Schema:      idArg,
			Detail:      idDetail,
			Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
				var a struct {
					ID string `json:"id"`
				}
				if err := agenttools.Decode("agent", c.Args, &a); err != nil {
					return agenttools.Result{}, err
				}
				m, err := w.agentIn(ctx, a.ID)
				if err != nil {
					return agenttools.Result{Error: err.Error()}, nil
				}
				b, _ := json.Marshal(m)
				return agenttools.Result{Text: string(b)}, nil
			},
		},
		{
			Name:        "stop_agent",
			Description: "Interrupt a background agent you started.",
			Schema:      idArg,
			Detail:      idDetail,
			Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
				var a struct {
					ID string `json:"id"`
				}
				if err := agenttools.Decode("stop_agent", c.Args, &a); err != nil {
					return agenttools.Result{}, err
				}
				out, err := w.stopAgentIn(ctx, a.ID)
				if err != nil {
					return agenttools.Result{Error: err.Error()}, nil
				}
				return agenttools.Result{Text: out}, nil
			},
		},
	}
}

func (w *Workers) nativeSpawn(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
	var a struct {
		Task       string `json:"task"`
		Background bool   `json:"background"`
		Project    string `json:"project"`
		Model      string `json:"model"`
	}
	if err := agenttools.Decode("spawn", c.Args, &a); err != nil {
		return agenttools.Result{}, err
	}
	if c.Worker != "" {
		return agenttools.Result{Error: "workers: subagent depth 1 only"}, nil
	}
	if a.Background {
		m, err := w.startAgent(ctx, a.Task, a.Project, a.Model)
		if err != nil {
			return agenttools.Result{Error: err.Error()}, nil
		}
		b, _ := json.Marshal(m)
		return agenttools.Result{Text: string(b), Data: map[string]any{"session": m["session"]}}, nil
	}
	if a.Project != "" || a.Model != "" {
		return agenttools.Result{Error: "workers: project and model are for a background agent; add background: true"}, nil
	}
	if strings.TrimSpace(a.Task) == "" {
		return agenttools.Result{Error: "workers: spawn needs a non-empty task"}, nil
	}
	// Resolved per call, off the apply goroutine, so the engine row is
	// not a remount edge of this one; under the loop there is none.
	eng, err := kernel.Get[spawner](w.kctx, "engine")
	if err != nil {
		return agenttools.Result{Error: "workers: a native spawn needs the engine-unreal row"}, nil
	}
	w.mu.Lock()
	if w.spawns >= w.maxSpawns {
		w.mu.Unlock()
		return agenttools.Result{Error: fmt.Sprintf("workers: spawn limit reached (%d per turn) — do the remaining work yourself in this turn", w.maxSpawns)}, nil
	}
	w.spawns++
	w.nextID++
	id := w.nextID
	w.mu.Unlock()
	worker := fmt.Sprintf("subagent %d", id)
	reply, status, steps, err := eng.Spawn(ctx, a.Task, worker, nativeSubSystemPrompt, w.maxSteps)
	data := map[string]any{"worker": id, "status": status, "steps": steps}
	if err != nil {
		return agenttools.Result{Error: fmt.Sprintf("workers: %s: %v", worker, err), Text: reply, Data: data}, nil
	}
	// Provenance, as tools.spawn gives it: delegated findings read as
	// delegated.
	text := fmt.Sprintf("[%s · task: %s]\n%s", worker, oneLine(a.Task, 80), reply)
	switch status {
	case "done":
	case "budget":
		text += fmt.Sprintf("\n[stopped at the step budget (%d); the report may be partial]", w.maxSteps)
	default:
		return agenttools.Result{Error: fmt.Sprintf("workers: %s ended %s", worker, status), Text: text, Data: data}, nil
	}
	return agenttools.Result{Text: text, Data: data}, nil
}
