// Package toolreg is the harness tool.Registry an engine coordinator
// sees: every agent-tools tool becomes a translator that submits ONE
// remote job (plan bough.call v1), which boughcall runs in-process with
// the same Go function the tool's row binds into codemode.
//
// Translators are pure: no I/O, no hooks, no clock. Everything that
// touches the world happens inside the op (boughcall), so a restore
// replays a session byte for byte whatever the tools do now.
package toolreg

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"encoding/json/jsontext"
	"fmt"
	"sort"
	"strings"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/tool"

	"github.com/andreylukin/bough/internal/agenttools"
)

// PlanType and PlanVersion name the one remote job every bough tool
// call is. A change to Plan or Handle gets a new PlanVersion, and
// Render keeps reading every version that ever shipped: stores outlive
// binaries.
const PlanType operation.RemoteJobPlanType = "bough.call"
const PlanVersion operation.RemoteJobPlanVersion = 1

// ViewImageName is the one harness tool bough keeps, under a
// snake_case name like the rest of the tool set.
const ViewImageName = "view_image"

// Plan is RemoteJobPlan.Data.
type Plan struct {
	Call string          `json:"call"`
	Tool string          `json:"tool"`
	Args json.RawMessage `json:"args"`
}

// Handle is RemoteJobState.Handle, written by boughcall on terminal updates.
type Handle struct {
	Detail    string         `json:"detail,omitempty"`
	Data      map[string]any `json:"data,omitempty"` // Result.Data
	Error     string         `json:"error,omitempty"`
	MS        int64          `json:"ms,omitempty"`
	Spill     string         `json:"spill,omitempty"`
	Truncated bool           `json:"truncated,omitempty"`
}

type Config struct {
	Tools     []agenttools.Tool // the snapshot; frozen for this coordinator
	ViewImage tool.Translator   // viewimage.New(viewimage.Config{Directory: cwd or orb Root})
	MaxOutput int               // op MaxOutputLength (≤ operation.MaxOutputLength)
}

// New builds the registry for one coordinator. Resolve is always true
// (see tombstone), so a restore never fails on a tool the session no
// longer has.
func New(c Config) tool.Registry {
	limit := c.MaxOutput
	if limit <= 0 {
		limit = operation.DefaultMaxOutputLength
	}
	limit = min(limit, operation.MaxOutputLength)
	r := &registry{byName: map[string]tool.Translator{}, viewImage: c.ViewImage}
	for _, t := range c.Tools {
		if _, dup := r.byName[t.Name]; dup {
			continue
		}
		r.byName[t.Name] = callTranslator{tool: t.Name, limit: limit}
		r.defs = append(r.defs, tool.Definition{Tool: ullm.Tool{
			Type:        ullm.ToolFunction,
			Name:        t.Name,
			Description: t.Description,
			Parameters:  t.Schema,
		}})
	}
	if c.ViewImage != nil {
		if _, taken := r.byName[ViewImageName]; !taken {
			r.byName[ViewImageName] = c.ViewImage
			r.defs = append(r.defs, viewImageDefinition())
		}
	}
	// One order for the tools block whatever order the snapshot came
	// in: the block is part of the cached prefix.
	sort.Slice(r.defs, func(i, j int) bool { return r.defs[i].Tool.Name < r.defs[j].Tool.Name })
	return r
}

func viewImageDefinition() tool.Definition {
	return tool.Definition{Tool: ullm.Tool{
		Type:        ullm.ToolFunction,
		Name:        ViewImageName,
		Description: "Look at a local image file (png, jpeg, gif, webp, bmp, tiff): it is attached to the result so you can see it. `view` answers with text only.",
		Parameters: agenttools.Object([]string{"path"}, map[string]any{
			"path": agenttools.Prop("string", "Image path, absolute or relative to the working directory."),
		}),
	}}
}

type registry struct {
	byName    map[string]tool.Translator
	defs      []tool.Definition
	viewImage tool.Translator
}

func (r *registry) StaticDefinitions() []tool.Definition {
	return append([]tool.Definition(nil), r.defs...)
}

func (r *registry) Resolve(name string) (tool.Translator, bool) {
	if t, ok := r.byName[name]; ok {
		return t, true
	}
	return tombstone{name: name}, true
}

// bough keeps its own skills mechanism (docs §11.3); the harness's
// SkillUse is never registered, so these are inert.
func (r *registry) RegisterSkill(tool.Skill) (tool.RegistrationID, error) {
	return tool.RegistrationID{}, fmt.Errorf("toolreg: skills are bough's, not the harness's")
}
func (r *registry) UnregisterSkill(tool.RegistrationID) {}
func (r *registry) Skills() []tool.Skill                { return nil }

// callTranslator submits one bough.call op for its tool.
type callTranslator struct {
	tool  string
	limit int
}

func (t callTranslator) Translate(ctx tool.Context, call ullm.ToolCall) tool.CallStatus {
	args := strings.TrimSpace(call.Arguments)
	// A no-argument tool arrives as "" from some providers.
	if args == "" {
		args = "{}"
	}
	if !jsonObject(args) {
		return tool.ErrorStatus("arguments must be a JSON object", t.limit)
	}
	data, err := json.Marshal(Plan{Call: call.CallID, Tool: t.tool, Args: json.RawMessage(args)})
	if err != nil {
		return tool.ErrorStatus(fmt.Sprintf("encode %s call: %v", t.tool, err), t.limit)
	}
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{Type: PlanType, Version: PlanVersion, Data: jsontext.Value(data)})
	if err != nil {
		return tool.ErrorStatus(fmt.Sprintf("build %s call: %v", t.tool, err), t.limit)
	}
	spec.MaxOutputLength = t.limit
	return tool.CallStatus{WaitingFor: []operation.ID{ctx.Submit(spec)}}
}

func (t callTranslator) TranslateResult(callID string, status tool.CallStatus, ops []operation.Operation) (ullm.ToolResult, error) {
	return result(callID, status, ops), nil
}

// tombstone stands in for any name the snapshot does not have: a tool
// retired since a call was recorded, or one the model made up.
type tombstone struct{ name string }

func (t tombstone) Translate(tool.Context, ullm.ToolCall) tool.CallStatus {
	return tool.ErrorStatus(fmt.Sprintf("tool %q is not available in this session", t.name), 0)
}

func (t tombstone) TranslateResult(callID string, status tool.CallStatus, ops []operation.Operation) (ullm.ToolResult, error) {
	return result(callID, status, ops), nil
}

func result(callID string, status tool.CallStatus, ops []operation.Operation) ullm.ToolResult {
	text, _, _ := Render(callID, status, ops)
	return ullm.ToolResult{CallID: callID, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: text}}}
}

func jsonObject(s string) bool {
	var v map[string]json.RawMessage
	return json.Unmarshal([]byte(s), &v) == nil && v != nil
}

// Render is the pure result text for a call, shared by TranslateResult
// and the projector. It never fails: an error from a translator kills
// the coordinator's Run, so anything unreadable becomes text the model
// can read instead.
func Render(callID string, status tool.CallStatus, ops []operation.Operation) (text string, h Handle, terminal bool) {
	if status.Error != "" && len(ops) == 0 {
		return "Error: " + status.Error, Handle{Error: status.Error}, true
	}
	if len(ops) == 0 {
		return "Error: no result was recorded for this call", Handle{Error: "no result was recorded for this call"}, true
	}
	op := ops[0]
	switch op.Status {
	case operation.StatusReady, operation.StatusAwaiting, operation.StatusCanceling:
		return contextbuilder.ToolCallRunningPayload, Handle{}, false
	}
	if op.Type != operation.TypeRemoteJob {
		// Only a bough.call op reaches here from a bough translator; a
		// harness op under a tombstone (view_image with none configured)
		// still reads as its status.
		return fmt.Sprintf("Error: %s operation %s", op.Type, op.Status), Handle{Error: string(op.Status)}, true
	}
	state, err := operation.DecodeRemoteJobState(op)
	if err != nil {
		msg := fmt.Sprintf("unreadable call state: %v", err)
		return "Error: " + msg, Handle{Error: msg}, true
	}
	if len(state.Handle) > 0 {
		// A Handle this version cannot read keeps the text; the row loses
		// its extras, the model loses nothing.
		_ = json.Unmarshal(state.Handle, &h)
	}
	switch op.Status {
	case operation.StatusCompleted:
		return state.TerminalResult, h, true
	case operation.StatusFailed:
		return "Error: " + state.TerminalError, h, true
	case operation.StatusCanceled:
		if h.Error != "" {
			return "Cancelled: " + h.Error, h, true
		}
		return "Cancelled", h, true
	}
	return fmt.Sprintf("Error: call in unknown state %q", op.Status), h, true
}

// Hash identifies a tool set by what the model is shown: name,
// description and schema. The engine restarts its coordinator at idle
// when it changes, because the tools block is part of the prompt.
func Hash(tools []agenttools.Tool) string {
	sorted := append([]agenttools.Tool(nil), tools...)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].Name < sorted[j].Name })
	h := sha256.New()
	for _, t := range sorted {
		schema, _ := json.Marshal(t.Schema) // map keys sort, so the bytes are stable
		fmt.Fprintf(h, "%d:%s\x00%d:%s\x00%d:%s\x00", len(t.Name), t.Name, len(t.Description), t.Description, len(schema), schema)
	}
	return hex.EncodeToString(h.Sum(nil))
}

// Detail is the call-row text for a call of one of tools: the tool's
// own Detail, or "" when it has none or the name is unknown. It is the
// projector's Config.Detail, built from the same snapshot as New.
func Detail(tools []agenttools.Tool) func(name string, args json.RawMessage) string {
	by := map[string]func(json.RawMessage) string{}
	for _, t := range tools {
		if t.Detail != nil {
			by[t.Name] = t.Detail
		}
	}
	return func(name string, args json.RawMessage) string {
		if name == ViewImageName {
			var a struct {
				Path string `json:"path"`
			}
			_ = json.Unmarshal(args, &a)
			return a.Path
		}
		if d := by[name]; d != nil {
			return d(args)
		}
		return ""
	}
}

// DecodePlan reads a bough.call op's plan; ok is false for any other op.
func DecodePlan(op operation.Operation) (Plan, bool) {
	if op.Type != operation.TypeRemoteJob {
		return Plan{}, false
	}
	state, err := operation.DecodeRemoteJobState(op)
	if err != nil || state.Plan.Type != PlanType || state.Plan.Version != PlanVersion {
		return Plan{}, false
	}
	var p Plan
	if json.Unmarshal(state.Plan.Data, &p) != nil {
		return Plan{}, false
	}
	return p, true
}
