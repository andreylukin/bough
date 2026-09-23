# engine-unreal: unreal-agent as bough's agent engine

Status: implementation contract, 2026-09-22. Base: `main` at ce6f5c23.
Harness pin: `github.com/unreallabsai/unreal-agent` **v0.1.1 =
b7c9bf1c5c2fa4127255c07727a7c8413e23944a** (the module cache's
`v0.1.1.info` carries that Origin.Hash, so `go get …@b7c9bf1c…` writes
`v0.1.1` to go.mod and go.sum's h1 hash is the real pin).

Six engineers (W1–W6, §18) build from this document in parallel. A
section or code block marked **frozen** is an interface other
workstreams compile against: change it only additively, and name the
change in the commit body. Everything else belongs to the workstream
§18 assigns it to.

Where this comes from: three competing designs were judged, and the
best, "engine-unreal as the loop row's plugin, bough tools behind one
`bough.call` remote job", was taken whole with the judges' required
grafts from the other two (Latest-replay on re-Add, `hseq`, turn
settle, refusing a fork over running calls, the import boundary, the
`llm-script` row and `bough engine script`, `delta-reset`, the
observed-input unpark, the headless drain, the trace flag). The
runtime-contract sections of `codex-engine-20260922a:go/docs/codex-engine.md`
(§5.1 keys, §7.1–7.4 event→history, `done` keys and ordering, §7.6
serve `inTurn`) are still accurate background; nothing Codex-specific
applies.

---

## 0. Decisions on one screen

1. **One plugin, `engine-unreal`, on the existing `loop` row.** Opt in
   with `--set loop.plugin=engine-unreal`, or an overlay row
   `- id: loop` / `plugin: engine-unreal`. The loop and the engine can
   never mount together, because they provide the same keys under the
   same row id. **The base `go/bough.yml` keeps `plugin: loop` at the
   end of this run**; flipping the default is the user's call (§19).
2. **Same keys, same types.** The engine provides `prompt-sections`
   (`*loop.Sections`), `runner`, `inputs` (`chan string`, cap 8),
   `cancel` (`func()`), `steer` (`func(string) bool`), plus two new
   keys, `engine` and `drain`. ui, serve, the web app, commands and
   history compile and run unchanged at that seam.
3. **One unreal coordinator per bough session**, inside a session
   Runtime that outlives coordinator restarts and engine-row remounts.
   The store and the operation manager live as long as the session;
   the coordinator, builder, inbox and tool-registry snapshot are
   rebuilt, lazily, at the next input.
4. **The `llm` row stays the source of truth** for provider, model and
   effort. The engine reads it per model request through a new
   optional seam, `agentllm.Source`, with a `kernel.Get` outside Apply
   (untracked, so no remount edge). `/model`, `/think`, the web pickers,
   `modelresume.go` and cost keep working, and none of them restarts
   the coordinator.
5. **bough's tools are native unreal tools behind ONE operation type**:
   remote_job plan `bough.call` v1. An in-process `RemoteJobHandler`
   (`internal/unreal/boughcall`) runs the same Go functions the tool
   rows run today. **That handler is the orb seam**: tools-basic's
   lazy `orb` lookup, `orb.Command` with `guestKiller`, the
   `errOrbNotReady` refusal and the write roots are reused, not
   re-implemented. The harness Shell op, SkillUse and a proxy manager
   are not used. The one harness tool kept is `view_image`.
6. **history.Entry stays canonical.** A projector turns observed store
   items into the kinds serve, the web and the TUI already read. Every
   projected entry carries `data.hseq`, the harness item Sequence, and
   projection catches up from the highest `hseq` when a session opens.
   New kinds: `call-delta` and `delta-reset` (live events only) and
   `engine` (a quiet entry).
7. **A bough turn (input → done) closes when the coordinator is
   quiescent and no foreground call is running.** A foreground call
   holds the turn open for at most `turn_settle` (60s) of idle waiting.
   After that each still-running call is adopted as a numbered job
   (jobs.go numbering, strip and `/jobkill`), and the turn closes with
   `done{running:N}`. The call's completion wakes the model in a *wake
   turn* whose `done{wake:true}` headless does not count against
   `hlPending`. `ask`/`secret` calls never settle: they hold the turn,
   as `tools.ask` does today.
8. **The coordinator's `llm.Adapter` is a bough Gate.** A user cancel,
   a provider error, a context overflow or a budget stop becomes an
   empty completed Response plus a bough entry. The model path
   therefore never ends `Coordinator.Run`. After a cancel the Gate
   *parks* the calls it cancelled, and is unparked by an observed input
   ID, synchronously in the store Observer. StopHard is never sent to
   a main-session coordinator.
9. **The session-lifetime operation manager remembers the latest
   snapshot of every op and re-emits it when a new coordinator re-Adds
   a known ID.** Without that, a terminal update consumed by a dying
   coordinator is lost for good. Verified in the pin:
   `LocalOperationManager` drops a duplicate Add silently
   (`harness/operation/local_manager.go:133`), and `slurpChannel`
   returns on ctx.Done after it has already dequeued updates
   (`harness/coordinator/loop.go:198`, `:438`).
10. **Anthropic goes over the native Messages API**, in
    `internal/messagesapi` (anthropic-sdk-go v1.71.0, beta Messages,
    streaming). OpenAI, OpenRouter and Ollama use the harness
    `responsesapi` adapter, which bough constructs over a tapped
    `http.Client` so deltas stream. No fork of unreal-agent.
11. **Every `Reasoning.Raw` and `ProviderID` goes through a provenance
    envelope**, so switching provider or model mid-session drops what
    the new provider cannot read and never sends it foreign bytes.
12. **The system prompt is frozen per coordinator.** Edits to
    AGENTS.md, MEMORY.md or a prompt section reach the model as a
    `<context-update>` block prepended to the next input. Input[0] is
    never edited, which keeps the prompt cache and the Opus 5.5 /
    Fable 5.1 preserved-thinking check intact.
13. **Code mode.** The loop, which is pure code mode, stays the product
    default. On the engine, native tools are the default and `run_js`
    is opt-in (`tools: both`). A same-task bench decides any flip
    (§13).
14. **Nothing is deleted in this run.** The removal list (§17) is
    ordered and gated on the flip and on the user.

## 1. Scope of this run

**In:**
- the pin;
- the Anthropic, OpenAI, OpenRouter and Ollama adapters, `llm-echo`
  and `llm-script` on the engine;
- the engine row and its session runtime;
- native tools for everything the loop exposes except `scratch`
  values (`run_js`-only);
- orb execution;
- the item→history projector;
- TUI, headless, serve and web readers for native call rows;
- hooks, skills, context-md, rules and optional native MCP tools on
  the engine;
- hermetic tests at every layer, and a live smoke script.

**Out:** the default flip, every deletion (§17), Bedrock/Vertex/Foundry
clients, server-side compaction, context editing, native MCP tools on
by default.

**Invariant for every workstream:** a session on `plugin: loop` behaves
byte-for-byte as it does on ce6f5c23. Every reader change is an
additive branch. Old history files still render, still resume on the
loop, and still resume on the engine through the seed path (§12.2).

Two deliberate exceptions, both in the llm rows the loop shares with
the engine. `max` is a level for `/think` and every row's `effort`; the
loop's OpenAI, OpenRouter and Cerebras paths fit it to the model's
catalogue entry and send xhigh where the catalogue does not know the
model, so no level main refused reaches a model that rejects it. And
`llm-anthropic` takes `effort` and answers `/think`: the loop's path
sends `output_config.effort` only once a level is set, so a row that
never sets one sends the bytes it sent on ce6f5c23, and an `effort`
value that is not a level fails the row, as it does on the others.

## 2. Topology and lifetimes

```
bough process (TUI, or `bough --headless --json [-r id]`, the child serve spawns)
└─ row `loop`, plugin engine-unreal          (plugins/engine)
   └─ session.Runtime   one per bough session; handed over across remounts
      │                 by history path (the loop's handoff pattern, loop.go:2315-2334)
      ├─ store     *localfile.Store, dir ~/.bough/engine, file <sid>.session.jsonl
      │            Observer added once, before any Run; it runs synchronously
      │            on the coordinator goroutine and only does O(1) work
      ├─ ops       *ops.Manager (session ctx) wrapping
      │            operation.NewLocalOperationManager(sessionCtx, boughcall.Handler)
      ├─ mirror    sync state machine, updated inside the Observer; the Gate reads it
      ├─ actor     ONE goroutine: projection, turn accounting, steer/notice queues,
      │            cancel, settle timer, history writes, event emits
      ├─ gate      the llm.Adapter the coordinator sees (§9.6)
      └─ run       nil until the first input; then inbox, builder (fresh), registry
                   snapshot, and a goroutine running coordinator.Run
```

**Harness session id (`sid`).** Take the bough session id (the history
file's base name) and replace every byte outside `[A-Za-z0-9-]` with `-`.
UUIDv7 ids pass through unchanged. The fallback
`20060102T150405.000Z-pid` becomes `20060102T150405-000Z-pid`, because
localfile rejects `.`. Subagent sessions are `<sid>-w<n>`. `Create` is
called only after `Resume` returns `fs.ErrNotExist`, because
localfile's `Create` overwrites an existing file.

**Paths**, all under `$HOME`, so tests with a `t.TempDir()` HOME stay
hermetic:

| What | Path |
|---|---|
| store | `~/.bough/engine/<sid>.session.jsonl` |
| frozen system prompt | `~/.bough/engine/<sid>.system.md` (written at the first build, never rewritten) |
| trace (`trace: true`) | `~/.bough/engine/trace/<sid>.jsonl` (keys redacted) |
| spilled call output | `$BOUGH_SCRATCH/calls/<callID>.out` (orb-mounted, same path in the guest) |

**Coordinator build**, lazily at the first input or notice after `Open`, a restart, or Run's death:
1. `store.Resume(sid)`, or `Create` on `fs.ErrNotExist`.
2. `inbox.New(runCtx, restored.ExternalInputIDs)`.
3. Compose the frozen prompt (§11.1) at the session's first build,
   or read `<sid>.system.md` if it exists.
4. `reg := toolreg.New(...)` from an agent-tools snapshot.
5. `b := prompt.Wrap(contextbuilder.NewBuilder(), frozen, prompt.Placeholder)`, then
   `b.SetModel(ullm.Model{ID: <row model now>})` and `b.AddTool(d.Tool)`
   for each `reg.StaticDefinitions()`.
6. `coordinator.New(coordinator.Dependencies{ToolHeartbeatInterval:
   cfg.Heartbeat, SessionID: sid, Inbox, Restored, Sessions: store,
   ContextBuilder: b, LLM: gate, Tools: reg, Operations: ops})`.
7. Record the quiet `engine` entry (§10.1), submit the input, and start
   `go run.Run(runCtx)`.

Interactive sessions never submit `StopWhenIdle`. Headless EOF goes
through `drain` (§9.10).

**Restart** always means cancelling Run's ctx. It is never StopHard, so
ops are untouched and the same `ops.Manager` replays their state. A
restart happens in three cases:
- The agent-tools set hash changed. The restart waits until the turn
  closes.
- Run returned an error (a store, Build, Add or translate failure).
  The actor records `error` and `done`, and the next input rebuilds.
- `/new`, `/sessions` or a history switch, which closes this Runtime.

**Process restart** (serve Stop is SIGINT → exit 130 → respawn with
`-r`) builds a fresh manager. boughcall fails any op re-Added in
`awaiting` with "interrupted: bough restarted before this call
finished; it was not re-run". That matches today, where serve Stop
kills the child.

## 3. Packages added

"Imports unreal" is enforced by the import-boundary test (§16).

| Path | Role | Owner | Imports unreal |
|---|---|---|---|
| `go/internal/agenttools` | **frozen** tool vocabulary + in-memory Registry + Hooks seam (§4.1) | B0, then W3 | no |
| `go/internal/agentllm` | **frozen** Source/Adapter/Delta vocabulary between the llm rows and the engine (§4.2) | B0, then W1 | yes (`harness/llm`) |
| `go/internal/messagesapi` | Anthropic Messages `ullm.Adapter`: `adapter.go` `request.go` (pure Render) `response.go` (pure Decode) `stream.go` `models.go` `errors.go` `trace.go` | W1 | yes |
| `go/internal/unreal/responses` | builds `responsesapi.NewAdapter` for openai / openrouter / ollama over `primitives.NewRemoteClientWithHTTPClient(tap(client))`; `tap.go` = SSE tee → deltas | W1 | yes |
| `go/internal/unreal/wrap` | adapter decorators: `Envelope`, `LateResultsAsText`, `StripReasoningOn400`, `Observe` | W1 | yes |
| `go/internal/unreal/fake` | scripted adapter + JSON script loader + `FromStore` (§14) | B0 core, then W1 | yes |
| `go/internal/unreal/echo` | deterministic echo adapter behind `llm-echo` on the engine | W1 | yes |
| `go/internal/unreal/prompt` | `Parts`, `Compose`, `Reminder`, the embedded `engine.md` preamble, `Wrap` (a contextbuilder decorator) | W5 | yes |
| `go/internal/unreal/hookbridge` | hooks service → `agenttools.Hooks` + lifecycle hooks (§11.4) | W5 | no |
| `go/internal/unreal/toolreg` | `tool.Registry`: agent-tools snapshot → `bough.call` translators, `view_image`, tombstones | W3 | yes |
| `go/internal/unreal/boughcall` | `operation.RemoteJobHandler` for `bough.call` v1: hooks, timeout, spill, redaction | W3 | yes |
| `go/internal/unreal/ops` | session-lifetime `operation.Manager` wrapper: Latest replay, coalescing queue, tap | W2 | yes |
| `go/internal/unreal/project` | pure projector: store item → `[]Out` (§10) | W4 | yes |
| `go/internal/unreal/session` | Runtime, mirror, actor, Gate, Children, fork, seed, catch-up | W2 | yes |
| `go/internal/unreal/contract` | tests only: behaviour of the pin that bough relies on (§15.2) | W2 | yes |
| `go/internal/unreal` (`doc.go`, `pin_test.go`, `boundary_test.go`) | pin + import-boundary tests | W6 | yes |
| `go/plugins/agenttools` | row `agent-tools`, provides the Registry | W3 | no |
| `go/plugins/engine` | row plugin `engine-unreal`: kernel glue, keys, config, handoff, `/context`, stored notices | W2 | yes |
| `go/cmd/bough/enginecmd.go` | `bough engine inspect\|reproject\|script` | W2 | yes |
| `go/plugins/llm/{agent,script,ollama}.go` | `AgentAdapter` on the llm rows, `llm-script`, `llm-ollama` | W1 | yes |

`internal/unreal/*` never imports another plugin to reach a service.
It may import `plugins/history` (Entry) and `plugins/llm` (Usage) as
vocabulary, exactly as the loop and the cost row already do. Services
arrive as funcs and interfaces in `session.Deps`, built by
`plugins/engine`. Events leave as `Emit(kind, text, data)`, and
`plugins/engine` wraps them in `loop.Event` on `"loop/event"`. The
title, activity, workers and cmux rows type-assert `p.(loop.Event)`
(title.go:287, activity.go:126, workers.go:697, cmux.go:272), so no
other struct may be emitted.

## 4. Frozen vocabulary (written in B0, verbatim)

### 4.1 `go/internal/agenttools/agenttools.go`, frozen

```go
// Package agenttools is the tool vocabulary shared by the tool rows and
// any engine that exposes them as native tool calls. A tool registered
// here is the same Go function its row already binds into codemode, so
// the loop and the engine run one implementation.
package agenttools

import (
	"context"
	"encoding/json"
)

// Tool is one native tool. Name is what the model calls; both Anthropic
// and OpenAI reject names outside ^[a-zA-Z0-9_-]{1,64}$.
type Tool struct {
	Name        string
	Description string
	Schema      map[string]any // JSON Schema, "type": "object"
	// Blocking marks a call whose wait is on the user (ask, secret): it
	// holds its turn with no settle, as tools.ask does today.
	Blocking bool
	// Detail is the call row's text: bash's first command line, the path
	// for write/patch/view, "path:start-end" for a ranged view.
	Detail func(args json.RawMessage) string
	Call   func(ctx context.Context, c Call) (Result, error)
}

type Call struct {
	ID       string          // provider call id; the call row's id
	Args     json.RawMessage // verbatim model arguments (a JSON object)
	Session  string          // bough session id
	Worker   string          // "" for the main agent, else the subagent's name
	Progress func(text string)
}

// Emit sends live output for the call row (a call-delta); nil-safe.
func (c Call) Emit(text string) {
	if c.Progress != nil {
		c.Progress(text)
	}
}

// Result is what the model reads plus what the call row shows. A
// non-empty Error (or a returned error) fails the call: the model reads
// "Error: "+Error, then Text when there is any.
type Result struct {
	Text  string
	Data  map[string]any // exit, add, del, job, path, cmd: copied onto the call entry
	Error string
}

type Registry interface {
	// Register fails, naming the tool, on an invalid or taken name.
	Register(t Tool) (unregister func(), err error)
	Lookup(name string) (Tool, bool)
	Tools() []Tool            // sorted by Name
	Changed() <-chan struct{} // closed and replaced on every Register/unregister
}

func NewRegistry() Registry // mutex-guarded map; the channel pattern is close-and-replace

// ValidName reports whether name is callable on every provider.
func ValidName(name string) bool

// Hooks is the tool half of the hooks row (implemented by
// internal/unreal/hookbridge). A nil Hooks runs every call unhooked.
type Hooks interface {
	// PreTool may refuse a call (deny != "": the model reads
	// "Error: blocked by hook: "+deny) or replace its arguments (args != nil).
	PreTool(ctx context.Context, tool string, c Call, detail string) (args json.RawMessage, deny string)
	// PostTool may rewrite what the model reads.
	PostTool(ctx context.Context, tool string, c Call, detail string, r Result) Result
}
```

### 4.2 `go/internal/agentllm/agentllm.go`, frozen

```go
// Package agentllm is the seam between the llm rows (which own keys,
// model and effort) and an engine that drives a harness adapter.
package agentllm

import (
	"context"
	"errors"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

type DeltaKind string

const (
	DeltaStart     DeltaKind = "start"      // a provider request began (the Gate emits it)
	DeltaText      DeltaKind = "text"       // → assistant-delta
	DeltaThinking  DeltaKind = "thinking"   // → thinking-delta
	DeltaToolStart DeltaKind = "tool_start" // → activity "writing <Name> call"
	DeltaRetry     DeltaKind = "retry"      // → system "provider hiccup — retrying…" (not recorded)
)

// Delta is one live fragment. Seq identifies the Gate's Respond call
// (SeqOf(ctx)); a consumer drops deltas from any Seq but the newest and
// resets its partial text when Attempt changes.
type Delta struct {
	Seq     uint64
	Attempt int
	Kind    DeltaKind
	Text    string
	CallID  string
	Name    string
	Wait    time.Duration // retry only
	Err     string        // retry only: the error being retried
}

// Exchange is one request/response pair for the trace file. Adapters
// never put credentials in it; the engine redacts again before writing.
type Exchange struct {
	Provider string
	Attempt  int
	Status   int
	Request  []byte
	Response []byte
	Err      string
}

type Options struct {
	Session string // harness session id; cache key and per-session adapter state
	Worker  string // "" for the main agent
	Sink    func(Delta)
	Trace   func(Exchange) // nil = off
}

// Adapter is a harness adapter an llm row built for one session.
type Adapter interface {
	ullm.Adapter
	Provider() string // envelope family: "anthropic", "openai", "openrouter:<vendor>", "ollama", "echo", "fake"
	Model() string    // the model id this adapter is calling right now
	Close() error
}

// Source is the optional seam on the "llm" service value. A row
// without it cannot drive the engine.
type Source interface {
	AgentAdapter(o Options) (Adapter, error)
}

type seqKey struct{}

func WithSeq(ctx context.Context, seq uint64) context.Context {
	return context.WithValue(ctx, seqKey{}, seq)
}

func SeqOf(ctx context.Context) uint64 {
	s, _ := ctx.Value(seqKey{}).(uint64)
	return s
}

// ErrContextOverflow is wrapped by adapters when the provider says the
// prompt no longer fits. The Gate makes it sticky until the model changes.
var ErrContextOverflow = errors.New("context window exceeded")
```

`Adapter.Model()` reads the row's live model. Each llm row builds its
adapter so that every `Respond` **overrides** `Request.Model.ID`,
`Model.ReasoningEffort` (mapped, §7.5) and `Model.MaxOutputTokens` from
the row's current state. The model the coordinator's builder carries is
a placeholder, and the harness `settings` control is not used.

## 5. Interfaces per package (frozen signatures)

B0 creates each package with a `stub.go` holding these signatures and
bodies that return `errNotYet("<pkg>.<Func>")` or zero values. The
owner replaces `stub.go` with real files. Unexported helpers are the
owner's business.

### 5.1 `internal/messagesapi` (W1)

```go
type Config struct {
	Client      anthropic.Client // option.WithMaxRetries(0); bough's bounded *http.Client; no overall timeout
	Model       func() string    // the llm row's live model
	Effort      func() string    // the llm row's live bough effort level
	MaxTokens   int64            // 0 = 64000, capped per ModelSpec
	CacheTTL    string           // "1h" (default) | "5m"
	Display     string           // "" = per-model table (§7.2)
	Fallbacks   string           // "auto" (default) | "off"
	Binding     string           // "" = none sent | "drop_block" | "error" (probes and tests)
	MaxAttempts int              // 0 = the §7.2 schedule
	IdleTimeout time.Duration    // 0 = 5m until probe P3 sizes it
	Options     agentllm.Options
}
func New(c Config) (agentllm.Adapter, error) // Provider() == "anthropic"
func Render(r ullm.Request, s ModelSpec, ttl string) (anthropic.BetaMessageNewParams, error) // pure
func Decode(m anthropic.BetaMessage) (ullm.Response, error)                                  // pure
type ModelSpec struct {
	ID         string
	Thinking   string   // "adaptive" | "budget"
	Efforts    []string // accepted output_config.effort values; nil = effort not sent
	MaxOutput  int64
	Display    string   // default display bough sends
	Betas      []anthropic.AnthropicBeta
	SystemMsgs bool     // mid-conversation role:"system" accepted
}
func Spec(model string) ModelSpec // prefix-keyed table; unknown ids get the Opus 5 row
```

### 5.2 `internal/unreal/responses` and `wrap` (W1)

```go
package responses
type Config struct {
	Kind        string        // "openai" | "openrouter" | "ollama"
	APIKey      func() (string, error)
	BaseURL     string        // "" = the kind's default
	Model       func() string
	Effort      func() string
	MaxTokens   int64
	CacheTTL    string        // openrouter Extensions cache_control ttl; "1h" default
	MaxAttempts int           // 0 = harness default 5
	HTTP        *http.Client  // bough's bounded client; tapped
	Options     agentllm.Options
}
func New(c Config) (agentllm.Adapter, error)

package wrap
func Envelope(a agentllm.Adapter) agentllm.Adapter            // §7.4
func LateResultsAsText(a agentllm.Adapter) agentllm.Adapter   // §7.3
func StripReasoningOn400(a agentllm.Adapter) agentllm.Adapter // sticky per instance, one retry
func Observe(a agentllm.Adapter, fn func(ullm.Response)) agentllm.Adapter
```

### 5.3 `internal/unreal/prompt` (W5)

```go
type Part struct{ Name, Text string }
type Skill struct{ Name, Description, Path string }
type Parts struct {
	Preamble     string   // engine.md, or the engine row's system_prompt
	Env          string   // cwd, platform, date (date frozen at build)
	Guidance     string   // task_guidance text when on
	Ask          string   // ask section when ask-answers is mounted
	SessionStart string   // session-start hook context (first build only)
	Context      []Part   // context-md Parts() (AGENTS.md, MEMORY.md, …), in order
	Sections     []Part   // prompt-sections, sorted by name
	Skills       []Skill  // catalogue
	Schema       string   // stop-schema SchemaSection
}
func Compose(p Parts) string                               // deterministic; same Parts → same bytes
func Hash(p Parts) string                                  // sha256 of Compose
func Reminder(prev, now Parts) (text string, changed []string) // "" when nothing changed
func Wrap(inner contextbuilder.Builder, system, placeholder string) contextbuilder.Builder
// Wrap.Build = inner.Build(); Input[0].Text = system; every ToolResult whose
// only output is contextbuilder.ToolCallRunningPayload gets placeholder text.
// SetSystemPrompt is forwarded but ignored. Pure and constant per coordinator.
const Placeholder = "This call is still running. Its result arrives later, possibly as a <tool_result call_id=…> block in a user message. Keep working on something independent, or end your reply to wait for it."
```

### 5.4 `internal/unreal/toolreg` and `boughcall` (W3)

```go
package toolreg
const PlanType operation.RemoteJobPlanType = "bough.call"
const PlanVersion operation.RemoteJobPlanVersion = 1
type Plan struct { // RemoteJobPlan.Data
	Call string          `json:"call"`
	Tool string          `json:"tool"`
	Args json.RawMessage `json:"args"`
}
type Handle struct { // RemoteJobState.Handle, written by boughcall on terminal updates
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
func New(c Config) tool.Registry
func Hash(tools []agenttools.Tool) string // name+description+schema; a change restarts at idle
// Render is the pure result text for a call, shared by TranslateResult and the projector.
func Render(callID string, status tool.CallStatus, ops []operation.Operation) (text string, h Handle, terminal bool)

package boughcall
type Options struct {
	Tools       func(name string) (agenttools.Tool, bool) // the live agent-tools registry
	Hooks       agenttools.Hooks                         // may be nil
	Redact      func(string) string                      // may be nil; applied before any state is written
	CallTimeout time.Duration
	SpillDir    func() string // $BOUGH_SCRATCH/calls
	Session     string
	Worker      string
	Progress    func(callID, text string) // → call-delta (coalesced by the actor)
}
func New(o Options) *Handler // implements operation.RemoteJobHandler
func (*Handler) RemoteJobPlanType() operation.RemoteJobPlanType
func (*Handler) RemoteJobPlanVersion() operation.RemoteJobPlanVersion
func (*Handler) AddRemoteJob(op operation.Operation) error
func (*Handler) CancelRemoteJob(id operation.ID, reason string) error
func (*Handler) RemoteJobUpdates() <-chan operation.Operation
func (*Handler) Close() error
```

### 5.5 `internal/unreal/ops` (W2)

```go
// Manager is the session-lifetime operation.Manager every coordinator of
// one session shares.
type Manager struct{ /* inner *operation.LocalOperationManager; latest map; queue; tap */ }
func New(ctx context.Context, inner *operation.LocalOperationManager, tap func(operation.Operation)) *Manager
func (m *Manager) Add(op operation.Operation) error // known id → re-emit Latest unless one is queued; else inner.Add
func (m *Manager) Cancel(id operation.ID, reason string) error
func (m *Manager) Updates() <-chan operation.Operation // one channel for the session; closed only when ctx ends
func (m *Manager) Latest(id operation.ID) (operation.Operation, bool)
```

The queue holds at most one pending snapshot per op id: the latest
wins, and order follows the first enqueue. That makes the re-Add replay
and a queued update coalesce instead of duplicating.

### 5.6 `internal/unreal/project` (W4)

```go
type Out struct {
	Kind   string
	Text   string
	Data   map[string]any // includes "hseq" on every Record entry
	Record bool           // true: history entry + event; false: event only
}
type Meta struct { // one per real or gated Respond, keyed by Response.ID
	ResponseID string
	Model      string
	Provider   string // the llm row's provenance name, as loop.go:819-834 records it
	Muted      bool   // the Gate answered without the provider
	Partial    bool   // a cancelled stream; Output holds its text
	Err        string // provider error the Gate converted
}
type Config struct {
	Prefix string // "" or "sub:"
	Worker string
	Detail func(tool string, args json.RawMessage) string
	Render func(callID string, status tool.CallStatus, ops []operation.Operation) (string, toolreg.Handle, bool)
	JS     string // name of the run_js tool ("" = none): its calls project as code/result
	RowOutput int // bytes of output kept on a call entry (head+tail)
}
func New(c Config) *Projector
func (p *Projector) Meta(m Meta)                      // must precede the ModelResponse it describes
func (p *Projector) Item(it sessionstore.Item) []Out  // content rows only (§10.2 column P)
func (p *Projector) Delta(d agentllm.Delta) []Out     // live-only
func (p *Projector) Progress(callID, text string) []Out
func (p *Projector) Call(callID string) (tool, detail string, ok bool)
```

### 5.7 `internal/unreal/session` (W2)

```go
type Deps struct {
	SessionID string // bough session id
	HistoryPath string
	Store     string // store dir
	Scratch   func() string
	Cwd       string
	Config    Config // §6.1, parsed

	LLM       func() (agentllm.Source, provenance string, err error) // lazy "llm" (else error naming the row)
	Usage     func() llm.Usage       // lazy "usage", else llm's UsageReporter
	Tools     agenttools.Registry
	Hooks     func() agenttools.Hooks // hookbridge over the lazy "hooks" key; may return nil
	Lifecycle func() Lifecycle        // hookbridge lifecycle half; may return nil
	Prompt    func() prompt.Parts     // everything but Preamble/Guidance, read lazily
	Orb       func() (root string, ok bool)
	Redact    func(string) string
	Jobs      func() Jobs             // lazy "job-notices" (tools-basic)
	Stats     func() TurnStats        // lazy "turn-stats"
	Checkpoints func() Checkpointer   // lazy "checkpoints"
	Skills    func() Skills           // lazy "skills": Inject(input) []string
	History   History                 // Append, Entries, Path
	Emit      func(kind, text string, data map[string]any)

	// Tests only: override what toolreg/boughcall/project would build.
	Registry  func(sid string) (tool.Registry, []operation.RemoteJobHandler)
	Projector func(prefix, worker string) *project.Projector
}
type Lifecycle interface {
	SessionStart(ctx context.Context) (context string)
	PromptSubmit(ctx context.Context, text string) (out string, block string, contexts []string)
	Stop(ctx context.Context, reply string) (continueWith string)
	Drain() []map[string]any // hook fire records → "hook" entries
}
type Jobs interface {
	Take() []string
	Wake() <-chan struct{}
	Adopt(cmd, call string, kill func()) (id int, finish func(exit *int, stopped bool))
}
type TurnStats interface{ Take() (files []string, exit int, ran bool) }
type Checkpointer interface {
	Snapshot() string
	Pin(seq int64, tree string)
	Changed(before string) []string
}
type Skills interface{ Inject(input string) []string }
type History interface {
	Append(kind string, data map[string]any) history.Entry
	Entries() []history.Entry
	Path() string
}

func Open(ctx context.Context, d Deps) (*Runtime, error) // Resume-or-seed + catch-up; no coordinator yet
func (r *Runtime) Submit(line string)          // the inputs chan
func (r *Runtime) Steer(text string) bool      // false when no bough turn is open
func (r *Runtime) Cancel()                     // Esc / SIGINT
func (r *Runtime) Drain(ctx context.Context) error
func (r *Runtime) CancelCall(callID string) error
func (r *Runtime) Context() string             // /context
func (r *Runtime) Children() Children
func (r *Runtime) Close(ctx context.Context) error // flush projection, stop Run, cancel the session ctx

type Children interface {
	Run(ctx context.Context, req ChildRequest) (ChildResult, error)
}
type ChildRequest struct {
	Task, Worker string
	System       string   // extra system text (the worker section)
	MaxSteps     int
	Allow        []string // tool allowlist; nil = parent tools minus spawn, agent, stop_agent, ask, secret
}
type ChildResult struct {
	Reply  string
	Status string // done | budget | cancelled | error
	Steps  int
}
```

### 5.8 Service keys the engine row adds (W2), frozen

| Key | Value | Readers |
|---|---|---|
| `engine` | `interface{ Session() string; StorePath() string; Spawn(ctx context.Context, task, worker, system string, maxSteps int) (reply, status string, steps int, err error); CancelCall(callID string) error }`. `Spawn` wraps `Runtime.Children().Run` with plain types, so a reader never imports `internal/unreal/*` (§16 boundary). | workers' `spawn` (W3), `bough engine` CLI |
| `drain` | `func(context.Context) error` | ui headless at stdin EOF (W4) |

Consumers declare the method set structurally, as `plugins/example`
does. They never import `plugins/engine`.

## 6. Rows, service keys, config

### 6.1 The engine row (W2)

`plugins/engine` registers `engine-unreal`. `Inject()` returns
`{"history", "agent-tools"}`. Nothing else is read during Apply,
because every Get during Apply becomes a remount edge (kernel/context.go:124-131).
The rest is read lazily, off the apply goroutine, at the moment it is
needed:
- per model request: `llm` (and `usage`);
- per turn: `hooks`, `skills`, `context-md`, `prompt-sections` text,
  `checkpoints`, `turn-stats`, `job-notices`, `stop-schema`,
  `ask-answers` (presence only), `session-mode`, `orb`;
- at Apply, `commands`, only to register `/context`. The loop does
  the same, so `commands` is a remount edge for both.

**Provides:** `prompt-sections` (a fresh `&loop.Sections{}`), `runner`,
`inputs`, `cancel`, `steer`, `engine`, `drain`.

**Handoff.** A package-level map is keyed by history path, the loop's
pattern at loop.go:2315-2334. A remount whose history path is unchanged
takes the same Runtime. A changed path Closes the old Runtime and opens
the new one lazily. On unmount without a remount (process exit), the
Runtime is Closed.

**Stored notices.** Every second the row polls for `notice` entries
and writes `notice-delivered {id}`. This is a copy of
`deliverStoredNotices` (loop.go:1066-1101) into
`plugins/engine/notices.go`; the loop's copy stays.

```yaml
- id: loop
  plugin: engine-unreal
  config:
    tools: native          # native | both      (both = native tools + run_js)
    turn_settle: 60s       # idle wait on foreground calls before they become jobs (§9.3)
    heartbeat: 0           # harness ToolHeartbeatInterval; 0 = off (a beat is a paid request)
    call_timeout: 10m      # deadline for every bough.call; bash's own `timeout` arg overrides
    max_output: 40000      # bytes the model reads per result (≤1000000); the rest spills
    row_output: 8192       # bytes of output kept on a call history entry (head+tail)
    max_steps: 100         # model requests per bough turn (the home overlay's 30 carries over)
    max_cost_usd: 0        # 0 = off
    stop_retries: 2        # stop-schema misses re-asked per turn
    steer_interrupts: false # true: a steer cancels the in-flight request (partial output discarded)
    system_prompt: ""      # replaces engine.md
    task_guidance: false
    store: ~/.bough/engine
    trace: false           # request/response bodies → ~/.bough/engine/trace/<sid>.jsonl
    # keep_whole_results: accepted from loop overlays and ignored, with a one-time
    #   system note, because the engine's context is append-only
```

Every config error names the row, for example
`fmt.Errorf("engine-unreal: turn_settle must be a duration like 60s, got %v", v)`.

### 6.2 The agent-tools row (W3)

A new base row, `- id: agent-tools / plugin: agent-tools`, sits right
after `commands` in `go/bough.yml` (W6 edits that file). It provides
`agent-tools` (an `agenttools.Registry`) and is harmless under the
loop. Each tool row gets `agent-tools` during its Apply and registers
its native tools there, in addition to its codemode bindings. It
unregisters them in an Effect. The codemode bindings stay byte-for-byte
the same, so the loop and `run_js` are unaffected.

### 6.3 llm rows (W1)

Every llm row that can drive the engine implements `agentllm.Source` on
its service value. The keys below are **added** and only
`AgentAdapter` reads them. The keys `model`, `effort` and `max_tokens`
keep their meaning.

| Row plugin | New keys (defaults) | AgentAdapter builds |
|---|---|---|
| `llm-anthropic` | `cache_ttl: 1h` (`1h`\|`5m`), `thinking_display: auto` (`auto`\|`summarized`\|`omitted`\|`updates`), `fallbacks: auto` (`auto`\|`off`), `block_binding: ""`, `max_attempts: 0`, `idle_timeout: 0` | `Observe(Envelope(messagesapi.New(…)))` |
| `llm-openai` | `base_url`, `max_attempts` | `Observe(Envelope(StripReasoningOn400(responses.New{Kind:"openai"})))` |
| `llm-openrouter` | `late_results: auto` (`auto`\|`text`\|`native`; auto = text for `anthropic/*`), `max_attempts`, `cache_ttl: 1h` | `Observe(Envelope(StripReasoningOn400([LateResultsAsText](responses.New{Kind:"openrouter"}))))` |
| `llm-ollama` (**new** plugin, `plugins/llm/ollama.go`) | `model` (required), `base_url: http://localhost:11434/v1` | `Observe(Envelope(responses.New{Kind:"ollama"}))`; `Complete` = one `Respond` with no tools; unpriced |
| `llm-echo` | none | `echo.New()` (§14.2) |
| `llm-script` (**new**, `plugins/llm/script.go`) | `script: <path>` (required) | `Observe(Envelope(fake.New(nil, steps…)))`, with the steps from `fake.Load(path)`; a load error names the row |
| `llm-cerebras`, init.js JS provider | none | no Source. A turn-level error, not fatal: `engine-unreal: llm row plugin %s cannot drive the engine (use llm-anthropic, llm-openai, llm-openrouter, llm-ollama or llm-echo)` |

`Observe` adds each Response.Usage into the row's own tally, which is
its `UsageReporter`. So the status bar, `/cost`, the cost row and
`done.usage` read the same numbers they read today:
- `InputTokens` stays inclusive;
- `CacheReadTokens` = `CachedInputTokens`;
- `CacheCreationTokens` = `CacheWriteInputTokens`;
- `LastInputTokens` = this request's `InputTokens`;
- OpenRouter's priced `cost` comes from `Usage.Raw`.

W1 adds `CacheWrite1hTokens int` to `plugins/llm.Usage`, read from
Anthropic's `Usage.Raw` `cache_creation.ephemeral_1h_input_tokens`,
and prices it at 2× input in `internal/models` (`CostCached1h`).

W1 also adds `"max"` to `llm.Efforts`. `plugins/llm/cachettl.go`
`CacheTTL` then has to follow the row's `cache_ttl`, and `done.usage`
carries `ttl` in seconds, so serve's `LastCache` and the title quiet
timer are right.

W1 fixes `plugins/llm/retry.go` `retryable()`: an in-stream
`overloaded_error` is an `*anthropic.Error` with StatusCode 200, so the
check must branch on `Type()`.

### 6.4 Other rows touched

| Row | Change | Owner |
|---|---|---|
| `tools` (tools-basic) | native `bash`, `view`, `write`, `patch`, `jobs`, `job`, `job_kill`; `Jobs.Adopt` (§8.4); the calls.go sink does NOT fire on the native path | W3 |
| `ask` | native `ask`, `secret` | W3 |
| `workers` | native `spawn` (foreground → the `engine` key's `Spawn`), `agent`, `stop_agent` | W3 |
| `artifacts` | native `artifact`, `artifact_patch`, `artifact_answers`, `artifact_guide` | W3 |
| `orb` | native `portal` (portal.go) | W3 |
| `lsp` | native `lsp` | W3 |
| `todo` | native `todo` | W3 |
| `example` | native `wordcount`; docs/PLUGINS.md shows it | W3 |
| `codemode` | `run_js` binding support (RunCtx under a per-session mutex) | W3 |
| `hooks` | payload keys for native tools (§11.4) | W5 |
| `mcp` | `native_tools: []` (server names, default none) → `mcp__<server>__<tool>` | W5 |
| `skills`, `context-md`, `rules` | read by the engine as they are, plus tests | W5 |
| `cost`, `internal/models` | 1h write pricing, `claude-opus-5-5` override | W1 |
| `ui`, `title`, `activity`, `cmux`, serve, web | readers (§10.4) | W4 |

## 7. Providers (W1)

### 7.1 Adapter stack

The coordinator calls the Gate (session, W2). For each `Respond` the
Gate:
1. assigns `Seq`;
2. resolves `Deps.LLM()`;
3. keeps one `agentllm.Adapter` per llm service instance, compared by
   identity. A `/model` remount of the llm row yields a new instance:
   the old adapter is Closed and a new one is built with
   `Options{Session: sid, Sink, Trace}`;
4. emits `Delta{Kind: start}` and forwards the request.

The row builds the stack in the order given in §6.3's table. Any adapter
error reaches the Gate, which never returns it to the coordinator
(§9.6).

### 7.2 Anthropic: `internal/messagesapi`

Before writing code, W1 loads the `claude-api` skill and reads its
`go/claude-api/README.md`, `streaming.md` and `tool-use.md`. SDK names
come from those files or from the SDK source in the module cache,
never from memory.

**Render** is a pure function of `ullm.Request` and `ModelSpec`:

1. **System prompt.** `Input[0]` is the system `Message`. It becomes
   `system: [{type: text, text, cache_control: {type: ephemeral, ttl}}]`
   (marker B1). A later `Role: system` Message does not occur today; if
   it does, render it as user text in `<context-update>`.
2. **Grouping.** Consecutive assistant-side items (assistant Message,
   ToolCall, Reasoning) form one assistant message, in item order.
   Consecutive user-side items (user Message, ToolResult) form one user
   message. The API rejects two same-role messages in a row, and an
   empty (muted) response adds no items, so two user runs separated
   only by it merge. The first message must be a user message. A render
   whose last message is from the assistant returns an error, because
   prefill is a 400 on every current model.
3. **Late and duplicate results.** A ToolResult becomes a `tool_result`
   block only when both hold:
   - it is the first ToolResult for its CallID;
   - it sits in the user message immediately after the assistant
     message that holds that CallID's `tool_use`.

   Such blocks go first in their user message, in item order. Any
   other ToolResult renders, at its own position in that user message
   after its `tool_result` blocks, as a text block followed by any image
   blocks:
   ```
   <tool_result call_id="toolu_…" name="bash">
   …output…
   </tool_result>
   ```
   Rendering is a deterministic function of position, so the transcript
   is append-only. It is the claude-api skill's reminder shape: text
   after the tool results, earlier copies kept.

   Rejected shapes:
   - rewriting the placeholder in place, which rewrites the cache and
     400s the preserved-thinking check on Opus 5.5 and Fable 5.1;
   - a second `tool_result` for the same id (a 400);
   - a synthetic `tool_use`.
4. **Missing results.** Any `tool_use` with no result in the next user
   message gets a synthesized
   `{type: tool_result, tool_use_id, is_error: true, content: "No result was recorded for this call."}`.
   All results for one assistant message are always in ONE user
   message, because splitting them teaches Claude to stop calling in
   parallel.
5. **Reasoning.** An unwrapped `Reasoning.Raw` whose `type` is
   `thinking` becomes `{thinking, signature}`, and `redacted_thinking`
   becomes `{data}`. Both are decoded from Raw and re-encoded through
   the SDK types, so the bytes are identical before and after a
   restore. Anything else is dropped. Every block is sent back
   unchanged in every earlier turn, and blocks are never stripped by
   hand (except for the one self-heal below).
6. **Tool calls.** A ToolCall becomes
   `{type: tool_use, id: CallID, name, input: json.RawMessage(Arguments)}`.
   The arguments never round-trip through a map, because key order is
   part of what the model generated. Invalid JSON becomes
   `{"invalid_arguments": "<raw>"}`.
7. **Images.** A ToolResult output of kind image, as
   `data:<mime>;base64,<b64>` (png or jpeg only), becomes a base64 image
   block. Anything else becomes the text `[image omitted: unsupported
   <mime>]`.
8. **Tools.** Each function tool becomes
   `{name, description, input_schema: Parameters}`, passed through
   `param.Override`. `strict` is off. `eager_input_streaming` is off:
   the harness acts only on the final message, so eager input buys
   nothing and costs validation, and older Bedrock deployments reject
   it. A hosted tool is an error naming the row.
9. **Parameters never sent:** `tool_choice` (forced choice is a 400 on
   Opus 5.5 and Fable 5.1), `temperature`, `top_p`, `top_k`, and
   prefill.
10. **Cache markers.** B1 goes on the system block. B2 goes on the last
    block of the user message just before the last assistant message,
    which is the previous request's final block, so a read is
    guaranteed despite the 20-block lookback. B3 goes on the last
    block. All three use the same TTL. The markers are explicit,
    because legacy Bedrock rejects the top-level field. That is three
    of the four slots.
11. **Thinking and effort**, from `Spec(model)`:

| Model prefix | `thinking` sent | `output_config.effort` | Notes |
|---|---|---|---|
| `claude-opus-5-5` | `{adaptive, display}` | always sent; `low`–`max` | disabled/budget → 400; default effort is medium, so always send it |
| `claude-fable-5-1`, `claude-fable-5` | `{adaptive, display}` | `low`–`max` | disabled/budget → 400; 5.1 needs 30-day retention (ZDR org → 400) |
| `claude-opus-5` | `{adaptive, display}` | `low`–`max` | never send disabled: it is only legal at ≤high and makes tool calls leak into text |
| `claude-opus-4-8`, `claude-opus-4-7`, `claude-sonnet-5` | `{adaptive, display}` explicitly | `low`–`max` | omitting thinking runs without it on 4.7/4.8 |
| `claude-opus-4-6`, `claude-sonnet-4-6` | `{adaptive}` | `low`/`medium`/`high`/`max` (xhigh → high) | |
| `claude-opus-4-5` | `{enabled, budget_tokens}` | `low`/`medium`/`high` | |
| `claude-haiku-4-5`, `claude-sonnet-4-5` | `{enabled, budget_tokens}` + beta `interleaved-thinking-2025-05-14` | not sent (400) | budget ≥1024 and < max_tokens |

**Display.** `thinking_display: auto` resolves per model:
- `updates` on Opus 5.5 and Fable 5 / 5.1. On Opus 5.5 this is what
  surfaces the between-call progress notes, which are otherwise empty
  thinking blocks. It needs beta `thinking-display-updates-2026-08-18`;
  W1 confirms per model in the claude-api `go/` docs, and probe P4
  confirms what comes back.
- `summarized` everywhere else. bough shows reasoning, and `omitted`
  (the default on 4.7 and later) looks like a hang.

Display stays fixed for a session, because changing it breaks the
cache and binding.

**Budgets** for budget models:

| Effort | budget_tokens |
|---|---|
| low | 2048 |
| medium | 8192 |
| high | 16384 |
| xhigh, max | 32768 |

Every budget is capped at `max_tokens - 1`.

12. **Fallbacks.** `fallbacks: auto` sends beta
    `server-side-fallback-2026-07-01` plus `fallbacks: "default"` on
    `claude-opus-5` and `claude-fable-5-1`, first-party only, as the
    claude-api skill prescribes. The actor records a one-time `system`
    note, "server-side fallbacks are on: a refused turn may be answered
    by another model at its rates". Decode drops a `fallback` block and
    every thinking, redacted_thinking or tool_use block before the last
    one (the echo rule). The default row, `claude-sonnet-5`, sends none.
13. **Max tokens.** `max_tokens` is `MaxOutputTokens`, else
    `Config.MaxTokens`, else 64000, capped at the model's max output
    (128K; Haiku 64K). The adapter always streams.

**Decode** runs on the message built by `BetaMessage.Accumulate`. Every
lossy decision is made here, once, and never again at render:

| Stream content | Harness item / field |
|---|---|
| `message.id` | `Response.ID` |
| signed `thinking` | `Reasoning{Summary: [text] if non-empty, Raw: block JSON}` |
| unsigned `thinking` | dropped |
| `redacted_thinking` | `Reasoning{Raw}` |
| non-empty `text` | assistant `Message` (citations dropped) |
| `tool_use` | `ToolCall{CallID: id, Name, Arguments: accumulated input JSON}` |
| `server_tool_use`, unknown blocks | dropped |
| `ProviderID` | always empty |
| stop `end_turn`, `tool_use`, `stop_sequence` | `StopComplete` |
| stop `max_tokens`, `model_context_window_exceeded` | `StopMaxOutputTokens`, and a trailing `tool_use` is dropped |
| stop `refusal` | `StopRefused`, every `tool_use` is dropped, `Failure{Code: "refusal:"+category, Message: explanation}` |
| stop `pause_turn` | error (only server tools produce it) |

Usage mapping:
- `InputTokens` = input + cache_read + cache_creation;
- `CachedInputTokens` = cache_read;
- `CacheWriteInputTokens` = cache_creation;
- `OutputTokens` = output;
- `ReasoningTokens` = `output_tokens_details.thinking_tokens`;
- `Raw` = the usage JSON, which keeps the 5m/1h split and iterations.

**Streaming** uses `NewStreaming` + `Accumulate`:

| Stream event | Delta sent to the Sink |
|---|---|
| `text_delta` | `DeltaText` |
| `thinking_delta` | `DeltaThinking` |
| `content_block_start` of type `tool_use` | `DeltaToolStart{CallID, Name}` |
| signature, input-JSON and citation deltas | accumulated only |
| `message_delta` | overwrites `output_tokens` (a running total), never adds |

The SDK drops `ping`, so the idle watchdog wraps the response body
through `option.WithMiddleware` and resets on every byte. A stream that
ends before `message_stop` is a retryable cut.

**Errors and retries.** The adapter owns every retry, because an error
it returns becomes a turn-level error in the Gate. It classifies with
`errors.As` into `*anthropic.Error` and branches on `Type()` and
status.

Retried:

| Error | Policy |
|---|---|
| 429 | wait `retry-after`, else the `anthropic-ratelimit-*-reset` headers, else backoff 5s→120s; about 15 attempts, as `plugins/llm/retry.go` `rateLimitDelays` |
| 529, and `overloaded_error` in-stream (status 200) | backoff 10s→60s |
| 500, 502, 503, 504, 408, EOF / cut stream, connection reset, TLS | up to 5 attempts |

Each retry emits `DeltaRetry{Attempt, Wait, Err}`.

Not retried:
- 400 `prompt is too long` → wrap `agentllm.ErrContextOverflow`.
- 400 naming an unknown `anthropic-beta` value → drop that beta for
  the process and retry once.
- 400 "thinking … bound to a different conversation" or "Invalid
  signature" → strip every thinking and redacted_thinking block that
  request carried and retry once. Those blocks stay out of later
  requests; blocks the model produces afterwards are bound to the
  stripped history and are replayed. This case is tested before the
  beta case: without the binding-controls header the message ends by
  naming the `anthropic-beta` header.
- Any other 400 → error.
- 401 → the `keyRejected` message from `plugins/llm/anthropic.go`
  `wrapErr`.
- 402, 403, 404, 413 → an error naming `llm-anthropic` and the model.
- A cancelled ctx returns `ctx.Err()` immediately.

**Client.** `anthropic.NewClient(option.WithAPIKey(key), option.WithHTTPClient(<bounded client>), option.WithMaxRetries(0))`.
The key comes from the same lookup `llm-anthropic` uses today
(`MissingKey` when unset). The bounded client in
`plugins/llm/httpclient.go` is passed in; it does not move.

### 7.3 OpenAI, OpenRouter, Ollama: `internal/unreal/responses`

bough constructs `responsesapi.NewAdapter(primitives.NewRemoteClientWithHTTPClient(tapped), responsesapi.Config{…})`
itself, because harness `clients/*.NewClient` hardcodes
`NewRemoteClient()`.

| Kind | Endpoint | Auth | Cache key | Extensions |
|---|---|---|---|---|
| openai | `<base or https://api.openai.com/v1>/responses` | `Authorization: Bearer` | `UsePromptCacheKeyField: true` | none |
| openrouter | `<base or https://openrouter.ai/api/v1>/responses` | Bearer | header `x-session-id` | `{"cache_control": {"type": "ephemeral", "ttl": "<cache_ttl>"}}` |
| ollama | `<base or http://localhost:11434/v1>/responses` | none | none | none |

These copy the harness `clients/{openai,openrouter,ollama}` configs at
the pin. A test asserts the request body for a fixed `ullm.Request` is
byte-identical to what the harness client sends.

**Tap.** A `RoundTripper` wraps a `text/event-stream` body in a reader
that splits SSE frames as `responsesapi` reads them:
- `response.output_text.delta` → `DeltaText`;
- `response.reasoning_summary_text.delta` → `DeltaThinking`;
- `response.output_item.added` with `type: function_call` →
  `DeltaToolStart`.

Every round trip increments `Attempt`, and `Seq` comes from the request
ctx. The tap is best effort and UI-only. If it breaks, GPT turns go
quiet until complete, but results stay correct. Probe P7.

**Late results.** OpenAI tolerates a second `function_call_output`, so
results stay native. OpenRouter with `anthropic/*` applies
`LateResultsAsText`: any non-first or misplaced ToolResult becomes a
user Message with the same `<tool_result call_id name>` text. Its
images become `[image omitted: call view_image again]`, because
`ullm.Message` carries no image. Probe P5 decides whether `native` is
ever safe.

`StripReasoningOn400`: a 400 whose message mentions reasoning,
thinking, signature or encrypted content strips Reasoning items and
retries once, sticky for the adapter instance. It is on for every
Responses kind.

### 7.4 Envelope (`wrap.Envelope`)

- **Out** (on Response): each `Reasoning.Raw` becomes
  `{"bough":1,"provider":"<Provider()>","item":<verbatim>}`, and each
  non-empty `ProviderID` becomes `<Provider()>|<id>`.
- **In** (on Request): an envelope with the same provider is unwrapped.
  A foreign provider, or an un-enveloped Raw, is dropped. A ProviderID
  with the same prefix is unwrapped; anything else is cleared.

Provenance is stamped at decode, not sniffed from bytes, so OpenAI
`encrypted_content` and OpenRouter reasoning can never be confused.
The envelope is persisted in the store, because Raw is opaque to the
harness, and it survives resume.

### 7.5 Effort mapping

bough levels are `llm.Efforts` plus `"max"`. The row's live `Effort()`
is clamped by `internal/models` `Efforts` for the model when the
catalogue lists them; unknown models pass through.

| bough | Anthropic adaptive | Anthropic budget models | OpenAI / OpenRouter / Ollama |
|---|---|---|---|
| `""` | effort not sent, except Opus 5.5, which gets `high`; adaptive still sent where the table requires it | thinking omitted | reasoning omitted |
| `off` | `low` (5.x cannot disable) | thinking omitted | `low` (the harness enum has no none) |
| `low` / `medium` / `high` | same value, clamped | 2048 / 8192 / 16384 | same value |
| `xhigh` | `xhigh` (4.6 → `high`) | 32768 | `xhigh` |
| `max` | `max` | 32768 | `max` if the catalogue lists it, else `xhigh` |

Changing effort mid-session invalidates the messages cache. That is
acceptable and is noted in `/think`'s reply. The per-message effort
beta is deferred.

### 7.6 Model catalogue bridge

- `messagesapi.Spec` holds the thinking and beta facts, which
  models.dev does not carry.
- `internal/models` keeps prices, context and efforts. W1 adds
  `overrides.go`, which is merged over the snapshot and the weekly
  cache. Its first entry is `anthropic/claude-opus-5-5`: input 4,
  output 20, cache read 0.20, 5m write 5.00, context 1M, efforts
  low–max.
- Also added: a `CacheWrite1h` field (json `cw1`), defaulting to 2×
  Input for anthropic models when absent, and `CostCached1h`.
- Merging the model picker with `GET /v1/models` is out of scope.

## 8. Tools and where they execute (W3)

### 8.1 Native tool set

| Tool | Args (JSON Schema object) | Row, function | Notes |
|---|---|---|---|
| `bash` | `command` (req), `timeout` (seconds or duration string), `background` (bool), `until` (regexp, background only) | tools-basic `bashRun` | Foreground deadline is `call_timeout` (10m), not 60s: the model is no longer blocked on it. `background:true` uses the jobs.go detach path (job_grace/job_settle from ce6f5c23, typed job entries, notice wake, `/jobkill N`) and returns at once. `Emit` streams stdout/stderr. `Data{exit, cmd}`. prefix_rule (rules) and lsp's bashNote run inside, unchanged. Detail = first line of command. |
| `view` | `path` (req), `start`, `end` | `viewFile` | Host-side as today; worktrees share host paths. An image path answers "this is an image; call view_image". |
| `write` | `path`, `content` | `writeFile` | Registered only where write roots allow (tools.go:518 rule). `Data{add, del, path}` via diffCounts. lsp afterEdit diagnostics and path-scoped rules notes are appended to Text. turn-stats `wrote()` feeds `done.files`. |
| `patch` | `path`, `old`, `new` | `patchFile` | same as write |
| `jobs`, `job` (`id`), `job_kill` (`id`) | | jobs.go | `jobWait` is not registered: a finished job's notice wakes the agent. |
| `ask` | `question`, `options` ([]string) | ask row, `Asker.Ask` | `Blocking: true`. The `ask` and `ask/answer` entries are unchanged. |
| `secret` | `name`, `question` | ask row `secret` | `Blocking: true`. It keeps today's contract: `projectdef.SetSecret` stores the value and the model reads "stored NAME as REF". The value never reaches `TerminalResult`, the op state, the store or the provider. |
| `spawn` | `task`, `background` (bool) | workers | Foreground: the `engine` key's `Spawn` (a child coordinator, §12.4). Background: `serveclient.CreateChild`, as today. `spawnAll` is not registered natively (parallel `spawn` calls do the same). |
| `agent`, `stop_agent` | as today | workers background agents | unchanged logic |
| `artifact`, `artifact_patch`, `artifact_answers`, `artifact_guide` | as the JS API | artifacts | A page answer arrives as a job notice and wakes the agent. |
| `portal` | `port`, `label` | orb/portal.go | Project sessions only. Registered at Apply whether or not the orb is ready yet; a call before it is ready gets `errOrbNotReady`. That way the tool set never changes when the orb comes up (§19 risk 2). |
| `lsp` | `op`, `path`, `line`, `col`, `query` | lsp | |
| `todo` | `op` (`add`\|`done`\|`clear`\|`list`), `text`, `id` | todo | Returns the list. Records todo/* and emits the `todo` event as today. The `cognition` section is NOT injected on the engine (it would edit the frozen prompt). |
| `wordcount` | `text` | example | the worked native example |
| `view_image` | `path` | harness `viewimage.New(Config{Directory: cwd or orb Root, Limits: harness defaults})` | The only harness tool, on a stock `view_image` op. Root is known at the first input because orb Prepare is synchronous (e99a8da5). |
| `run_js` | `code` | codemode `RunCtx` in the session VM | Only with `tools: both`. Parallel calls queue on a per-session mutex. `tools.*` inside it are today's synchronous bindings. Its per-call rows come from the calls.go sink (5ea62f89). The projector writes `code`/`result` for it. |
| `mcp__<server>__<tool>` | the server's input schema | mcp (§11.6) | Only for servers listed in `native_tools`. |

`scratch` values stay `run_js`-only. `$BOUGH_SCRATCH`, `/scratch` and
the orb mount stay.

Tool descriptions are W3's to write. They are short, and they say that
calls run asynchronously and that `bash` takes `background:true` for
servers and watchers.

### 8.2 `bough.call` lifecycle

**Translate** (`toolreg`, pure, no I/O):
1. The name must resolve to a snapshot tool.
2. The args must be a JSON object; if not, return
   `tool.ErrorStatus("arguments must be a JSON object")`.
3. `ctx.Submit(operation.NewRemoteJobSpec(RemoteJobPlan{Type: "bough.call", Version: 1, Data: Plan{call, tool, args}}))`,
   with `MaxOutputLength = max_output`.
4. Return `CallStatus{WaitingFor: [id]}`.

**Resolve always returns true:**
- a snapshot tool → the call translator;
- `view_image` → the harness translator;
- any other name → a **tombstone**. Its `Translate` returns
  `tool.ErrorStatus("tool %q is not available in this session")`, which
  also covers hallucinated names. Its `TranslateResult` renders any
  recorded `bough.call` op.

So restore can never fail with "tool … required by recorded call … is
not available", whatever the tool set becomes. A `bough.call` op never
changes Version. A plan or Handle shape change gets a new PlanVersion,
and `Render` keeps every version it ever shipped.

**TranslateResult / Render** (pure, never returns an error, because an
error kills Run). `DecodeRemoteJobState`, then:

| Op status | Text |
|---|---|
| ready / awaiting | the harness placeholder (the builder decorator swaps in `prompt.Placeholder`) |
| completed | `TerminalResult` |
| failed | `"Error: " + TerminalError` |
| canceled | `"Cancelled: " + reason` (from Handle.Error) |

**boughcall `AddRemoteJob`:**
- **Status awaiting** (a re-Add from a previous process):
  `FailRemoteJob("interrupted: bough restarted before this call finished; it was not re-run")`.
- **Status ready:** `UpdateRemoteJob(op, state, StatusAwaiting)`, then a
  goroutine runs these steps in order:
  1. Look up the tool. If it is missing, fail with "tool %q was removed
     while the call was queued".
  2. `Hooks.PreTool`. A deny fails with `blocked by hook: …`; args
     from the hook replace the model's.
  3. `Tool.Call(ctx)`. The ctx has `CallTimeout` unless the tool
     handles its own deadline (bash with a `timeout` arg).
     `Call.Progress` feeds `Options.Progress`.
  4. `Hooks.PostTool`.
  5. `Redact` the Text and the Error.
  6. If the text is over `MaxOutputLength`, write the whole text to
     `SpillDir/<callID>.out` and put head + tail + `…N bytes truncated;
     complete output in <path>…` into `TerminalResult`.
  7. `UpdateRemoteJob(completed | failed, Handle{detail, data, error, ms, spill, truncated})`.
- **Cancel** cancels that goroutine's ctx, then `CancelRemoteJob`.
  Every status transition must be legal to the manager's validator
  (ready→awaiting→terminal, awaiting→canceling→canceled).

Secrets and hooks run inside the op, before any snapshot exists. So
neither `SaveOperation` nor the provider ever sees a pre-redaction
string, and translators stay I/O-free.

### 8.3 Local vs project sessions

The engine executes nothing itself. Every exec goes through the tool
implementation, which reads the `orb` service lazily for each call
(tools.go:399-407).

| | Local session | Project session |
|---|---|---|
| bash | host, cwd = session cwd | `sh script` through `orb.Command` (Workdir = primary worktree). Cancel reaches the guest through `guestKiller`. Orb not ready → `errOrbNotReady`, never a host fallback. |
| write / patch | confined to `iorb.LocalWriteRoots()` ∪ session-project-dir; no roots, no tools | orb Root, `$BOUGH_SCRATCH`, `~/.bough/projects/<slug>` (projectMode.allowed) |
| view / view_image | host path | host path; worktrees have the same path in host and guest |
| env, secrets | inherited | the handle's execEnv + secretEnv; redaction through the orb Redactor |

Two rules must be carried over and tested: a project session never
falls back to the host, and a local session with no write roots gets
no `write`/`patch`. There is no isolation boundary to catch a slip.

**Fallback only.** If a project-session call ever has to use the
harness Shell op, the smallest seam is an `OrbShell` diversion inside
`ops.Manager`. It diverts TypeShell ops to `orb.Command` and emits a
terminal `ShellState` using its exported fields (shell.go:50-70) and
`operation.BoundOutput`. It is not built in this run.

### 8.4 Jobs: one numbering, one strip

W3 adds to `plugins/tools/jobs.go`:

```go
// Adopt numbers work the engine already runs (a call that outlived its
// turn's settle window) as a job, so the strip, /jobkill N and serve's
// job signals see it. It records job{id, event:"started", cmd, call}.
// No notice is ever queued for it: its result reaches the model through
// the engine. finish records job{id, event:"finished", exit?, stopped?, call}.
func (j *Jobs) Adopt(cmd, call string, kill func()) (id int, finish func(exit *int, stopped bool))
```

- `Running()` includes adopted jobs.
- `jobKill(id)` on an adopted job calls `kill`, which the engine wires
  to `Runtime.CancelCall(call)` → `ops.Cancel(opID)`.
- `jobs` and `job` show adopted jobs with the call's live output tail
  when one exists.

### 8.5 `run_js`

W3 builds this on the codemode row: `codemode.RunCtx` under a
per-session mutex. `Interrupt` is wired to the call's ctx. The block's
printed output is the Result Text, and a thrown error is `Result.Error`.
Inside the block, the calls.go sink emits `call`/`sub:call` exactly as
it does under the loop. The projector writes `code {text: code}` at
start and `result {text, code, ms, error?}` at the end, for readers that
need code→result pairs. `hookmeta.OnHost` suppression is unchanged.

## 9. Turn semantics (W2)

A harness "turn" is ONE model request (`ItemTurn`). A **bough turn** is
`input` → `done`, possibly spanning many harness turns. Everything
below is about keeping exactly one `done` per bough turn while the
harness runs every call asynchronously.

### 9.1 Mirror (`session/mirror.go`, pure)

`State.Apply(sessionstore.Item)` is O(1) and does no I/O. Two replicas
run the same code:
- **sync mirror**, applied inside the store Observer on the coordinator
  goroutine, before the coordinator's next step. The Gate reads it
  under a mutex.
- **actor mirror**, applied as the actor dequeues. The done and settle
  logic reads it, so a decision never runs ahead of the entries already
  written.

| Item | Effect |
|---|---|
| `ItemInput` external or heartbeat | `Pending++`, `reasons += input:<id>` |
| `ItemInput` control settings / stop | none |
| `ItemTurn` | `LastTurn = id`, `Inflight = true`, `TurnReasons = reasons`, `Pending = 0`, `reasons = {}` |
| `ItemModelResponse` for `LastTurn` | `Inflight = false`; each ToolCall → `Outstanding[callID] = {tool, blocking, firstAt: RecordedAt}` |
| `ItemToolCallStatus` with `Error`, or no `WaitingFor`, or all ops terminal | delete `Outstanding[callID]`, `Pending++`, `reasons += call:<id>` |
| any other `ItemToolCallStatus` | record op ids on the call |

Both replicas are seeded at `Open` by applying every store item in
order. The Observer does not fire during the coordinator's own
restore, so without this seeding the first `TurnReasons` after a
resume would be wrong. `ItemFork` resets nothing.

The mirror encodes the coordinator's own rule. The coordinator calls
the model iff `Pending > 0`, no request is in flight, and no grace
calls remain, and a status with `Error` or no `WaitingFor` calls it
at once. The contract tests (§15.2) pin that rule, including "a
completion inside the 1s grace plus a call still running at grace
expiry gives exactly one ItemTurn at ~1s".

### 9.2 Actor

The actor is one goroutine reading one unbounded FIFO. Everything that
changes bough-visible state enqueues onto it, so ordering is total:
- Observer items;
- Gate `Meta` and `Delta` values (the Gate owns the adapter Sink);
- boughcall `Progress`, coalesced to at most one event per call per
  100 ms, 4 KiB;
- `Submit`, `Steer`, `Cancel`, `CancelCall` and `Drain` requests
  (Steer and Drain wait for their reply);
- job-notices wakes;
- the settle timer;
- Run exits.

The Observer only applies the sync mirror, updates the Gate's parked
view and enqueues. It never writes history, so a slow fsync or event
fan-out can never stall the agent loop.

The actor tracks:
- `open` (a bough turn is open) and `turnStart`;
- `unobserved`, the set of input IDs submitted to the inbox and not yet
  dequeued as an ItemInput;
- `steerQueue`, `noticeQueue`, `notes` (text prepended to the next
  Submit);
- `adopted`, callID → job id and finish func;
- `turnCalls`, the calls issued in this bough turn;
- `cancelled`, calls cancelled by §9.7 that have not reached a terminal
  status yet; they never hold a later turn open;
- `turnRequests`, the non-muted ItemTurns in this bough turn;
- `settle`, the timer.

### 9.3 Closing a turn: the done rule, settle, adoption

After every dequeued event, the actor evaluates:

```
quiescent := open && !m.Inflight && m.Pending == 0 &&
             len(unobserved) == 0 && len(steerQueue) == 0 && len(noticeQueue) == 0
fg        := m.Outstanding minus adopted minus cancelled   // cancelled: §9.7 step 2
if !quiescent               { settle.Stop(); return }
if len(fg) == 0             { close(); return }
if any(fg, Blocking)        { settle.Stop(); return }   // ask/secret hold the turn
if !settle.armed            { settle.Reset(turn_settle) } // re-checked when it fires
```

**When the settle timer fires** and the state is still quiescent with
the same non-blocking `fg`:
1. For each call in `fg`, `Jobs.Adopt(detail, callID, kill)`. That
   records `job {id, event: started, cmd: detail, call}`.
2. `close(running: len(fg))`.

If the `job-notices` key is absent (no tools row), the actor records
the same `job` entries itself, with ids from its own counter starting
at 1000.

`close(extra)` runs these steps in order:
1. **Stop hook.** Call `Lifecycle.Stop(finalReply)`. A non-empty
   `continueWith` records `nudge {text}`, submits it as InputExternal,
   and returns with the turn still open (at most once per bough turn).
2. **stop-schema.** When the key is set, validate the final assistant
   text with the loop's rules (loop.go:1797-1855). A miss records
   `nudge {text}` and submits it (up to `stop_retries`); after that it
   records `error {text: "reply did not match the schema: …"}`.
3. Build `done` data exactly as loop.go `doneData` (1121-1164):
   - `files` = turn-stats `Take()` ∪ `cp.Changed(turnTree)` when a
     shell ran;
   - `exit` when bash ran;
   - `ms` = now − turnStart;
   - `after`;
   - `usage` = `loop.UsageDelta(prev, Usage())`, plus `ttl` in seconds.

   Then add the engine keys: `engine_turn`, the last `ItemTurn` id,
   which fork uses; `running`, when calls were adopted; `wake: true`,
   on a wake turn; `stop`, which is `max_steps`, `max_cost` or `error`
   when the turn ended on a budget or an error.
4. Record `done`, emit it, and set `open = false`.

A `done` is written only by `close`, and `close` runs at most once per
bough turn. So there is **exactly one `done` per bough turn**; a steer
or a mid-turn notice gets none; a cancel is always `cancelled` then
`done`.

### 9.4 Opening a turn

**Submit(line)**, from the `inputs` chan. The pipeline is the loop's
`admit` (loop.go:839-898) plus input dispatch, so a `!cmd` or `/…`
line is handled exactly as the loop handles it:
1. `Lifecycle.PromptSubmit`. A block records `error {text}` and
   `done`; a rewrite emits `context`.
2. `loop.ExpandAt(line, cwd)`. An image reference (pasted path) becomes
   the text "the user attached <path>; call view_image to look at it",
   because the harness has no user images.
3. `Skills.Inject(line)` blocks, each emitting `context`.
4. Prepend `notes`: queued steers left over from a cancel, and
   `<context-update>` reminders (§11.2).
5. `tree := cp.Snapshot()`.
6. `e := History.Append("input", {text, typed?, checkpoint: tree, input_id})`,
   then `cp.Pin(e.Seq, tree)`.
7. Set `open = true` and `turnStart = now`, and reset the per-turn
   counters.
8. `inbox.Submit(Input{ID: input_id, Kind: external, Payload: <JSON string of the admitted text>})`,
   building the coordinator first if there is none. Add `input_id` to
   `unobserved`.

**Notices** (`job-notices.Wake`; `Take()` on the actor):
- **No turn open:** emit `job {text: news}`, then Submit
  `jobWake + news` (the loop's constant, loop.go:401) with
  `input {…, wake: true, reason: "notice"}`. Its `done` carries
  `wake: true`.
- **Turn open:** record `job {text}` and submit the news as
  `[notice] …` by the steer rule (§9.5): at once when no request is in
  flight, else queued on `noticeQueue` until that request's
  `ItemModelResponse`. No extra `done`.

**Wake turns.** A `DeltaStart` from the Gate while no bough turn is
open means the coordinator woke on its own: an adopted job finished,
or a heartbeat fired. The actor then records
`input {text: "[background] job N finished: <detail>" (one line per reason), wake: true, reason: "call"|"heartbeat", calls: [ids], checkpoint}`,
emits `job`, and opens the turn. It uses `DeltaStart`, not `ItemTurn`,
so a muted request never opens a turn.

**Adopted job finishes.** The projector does not write a `call` end
row for an adopted call. The actor calls its `finish(exit, stopped)`,
which records `job {event: finished}`. The model reads the result in
the wake turn.

### 9.5 Steer

`steer(text)` is answered by the actor:
- **No turn open:** returns false, and ui sends the line to `inputs`.
- **Turn open:** records `input {text, steer: true, input_id}`, emits
  `steer`, and returns true.
  - A model request is in flight and `steer_interrupts` is false: the
    text goes on `steerQueue`. It is submitted right after that
    request's `ItemModelResponse` is dequeued. That keeps the loop's
    "steers land at boundaries" and never discards paid partial output.
  - Otherwise it is submitted now. If a request was in flight, the
    coordinator supersedes it. When the next request's first delta
    arrives, the projector emits `delta-reset {seq}` so the UIs drop
    the partial stream.

### 9.6 Gate (`session/gate.go`)

```go
func (g *Gate) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error)
```

1. `seq := ++g.seq`, then `ctx = agentllm.WithSeq(ctx, seq)`.
2. **Parked?** The Gate is parked from `Park` until its first unmuted
   request. While parked, if every reason in the sync mirror's
   `TurnReasons` is in `g.parkedCalls ∪ g.parkedInputs` (an empty set
   counts), return `Response{ID: "bough-muted-<seq>", Stop: complete}`
   and `Meta{Muted: true}`, without calling the provider. Otherwise
   un-park: clear both sets and go on.
3. **Sticky overflow**, for the same model: return an empty Response
   plus `Meta{Err: overflowText}`.
4. **Budgets.** The Gate counts its own unmuted requests, and
   snapshots `Usage().Cost`, at `g.TurnReset()`; the actor calls
   `TurnReset` whenever a bough turn opens. When the count reaches
   `max_steps`, or the turn's cost reaches `max_cost_usd > 0`, the Gate
   returns muted and enqueues a budget event. The
   actor then runs the cancel path with `stop` set and records
   `system {text: "step budget reached (max_steps N); stopped"}`
   instead of `cancelled`.
5. Resolve the adapter (§7.1) and emit `DeltaStart`. Call
   `inner.Respond(child, r, o)`, where `child` is a cancelable child of
   ctx held as `g.inflight`.
6. **Result:**

| Outcome | What the Gate returns |
|---|---|
| `ctx.Err() != nil` (the coordinator superseded it: a steer) | `ctx.Err()`; the coordinator drops it |
| `g.inflight` was cancelled by `Cancel()` | `Response{ID: "bough-cancel-<seq>", Stop: complete, Output: [assistant Message(partial text of this seq)] if non-empty}` and `Meta{Partial: true}` |
| error | `Meta{Err}`, an empty `Response{ID: "bough-error-<seq>"}`, nil error. `ErrContextOverflow` is sticky. |
| success | the response; `ID` = provider id or `bough-<seq>`; `Meta{Model, Provider}` |

A `Meta` is always enqueued before the Gate returns, so it precedes its
`ItemModelResponse`. The projector matches the two by `Response.ID`
(the harness never reads `Response.ID`).

**The partial text is kept in context on purpose.** The loop projects
a cut reply the same way, and the model should know where it was
stopped. Thinking from a cut stream has no signature and is dropped.

### 9.7 Cancel (Esc, `cancel()`, and SIGINT via main.go:534)

1. `g.Park(calls: turnCalls ∪ unconsumed completions not adopted, inputs: every input ID recorded so far)`,
   then `g.CancelInflight()`.
2. For every call in `turnCalls` still outstanding and not adopted:
   `ops.Cancel(opID, "cancelled by the user")`. `run_js` also gets
   codemode `Interrupt`, and foreground `spawn` children get their ctx
   cancelled.
3. Wait up to 1s for their terminal statuses, so their `call` rows
   (`canceled: true`) land before the close. It is 1s, not more,
   because main.go's `AwaitCancelled(3s)` bounds the whole SIGINT path,
   and `done` must be on disk inside it. A later one still records
   its `call` row (`late: true`); the web renders a lone call
   (app.tsx:1346).
4. Record `cancelled {}`, then `done` (with `stop` unset).
5. Move any steers still queued to `notes`.

Adopted jobs and background bash are untouched: "Esc cancels the turn
and its subagents, not background jobs." Their completions are not
parked, so they wake the model as today's jobWake does. Coordinator.Run
survives. The next input's ID is not parked, so the first request that
includes it goes to the provider, and the model sees the cancelled
results with it.

### 9.8 Ask

`ask` and `secret` are `Blocking` bough.calls. The handler goroutine
blocks in `Asker.Ask`, which records `ask {question, options, id, secret?}`
and emits the `ask` event. It is answered by a TUI key, a headless raw
line while the ask is armed, or serve's Answer; each records
`ask/answer {id, text}` and returns from the call. The result then wakes
the model in the same bough turn.

`heartbeat: 0` means a pending ask never triggers a paid request.
serve's `StatusOf` clears NeedsYou on `ask/answer`, which it already
reads (status.go:94); W4 adds the test.

### 9.9 Errors

- **Gate-converted provider errors:**
  1. Record `error {text}`, a turn-level failure, which is right for
     `StatusOf` and makes headless exit 1.
  2. Adopt the outstanding foreground calls as jobs AND park them, so
     their results reach the model with the next input instead of
     re-triggering a failing provider.
  3. `close(stop: "error", running: N)`.
- **Overflow:** the text is "the conversation no longer fits the
  model's context window; start a new session (/new) or fork from an
  earlier turn (/tree)". Auto-compaction stays a hard no, and nothing
  trims.
- **`Coordinator.Run` returning an error** (store, Build, Add or
  translate failure): record `error {text: "engine: " + err}`, and
  `done` if a turn is open. The coordinator is marked dead, and the
  next input rebuilds it. Ops keep running, and their updates queue in
  `ops.Manager` for the next coordinator.
- **Refusal:** record `error {text: "the model declined (<category>): <explanation>"}`.
  Only the model's own final reply ends the turn; tool calls in it were
  dropped at decode.
- **max_output_tokens:** record
  `system {text: "reply cut off at max_tokens; raise the llm row's max_tokens"}`.

### 9.10 Drain (headless EOF)

The `drain` key returns once `!open`, no outstanding calls (adopted
included), no in-flight request and no queued input, or when ctx ends.
ui headless calls it after `drainHeadless()` when the key exists, with
a ctx cancelled after `BOUGH_HEADLESS_IDLE` with no loop event (W4).
One-shot runs therefore let adopted calls and their wake turns finish.
Background bash (jobs.go) is not waited for, which matches today.

### 9.11 `done` accounting, as a property

W2's property test (rapid) drives the actor with random interleavings
of Submit, steer, cancel, notice, call completion, provider error,
steer-supersede and settle expiry, using a fake adapter and testkit
tools. It asserts:
- one `done` per Submit plus one per wake;
- `cancelled` is immediately followed by `done`;
- no `done` for a steer or a mid-turn notice;
- a headless counter that decrements on non-wake dones never goes
  negative and ends at 0;
- no provider request after a cancel until a new input.

## 10. Projection: store items → bough events and history

Events are change signals; history entries are the truth, as today.
Every recorded row also emits the event of the same kind, as a
`loop.Event{Kind, Text, Data}`.

### 10.1 Mapping table

The Emitter column says who produces the row: **A** is the actor
(`session`, W2) and **P** is the projector (`project`, W4). Every P row
that is recorded carries `data.hseq` = the item's Sequence.

| Source | E | Live event | History entry (data keys) |
|---|---|---|---|
| Submit (before `inbox.Submit`) | A | `context` per skill or rewrite | `input {text, typed?, checkpoint, input_id}` |
| steer | A | `steer {text}` | `input {text, steer: true, input_id}` |
| notice while idle | A | `job {text}` | `input {text: jobWake+news, wake: true, reason: "notice", input_id, checkpoint}` |
| notice mid-turn | A | `job {text}` | `job {text}` |
| wake (DeltaStart, no turn open) | A | `job {text}` | `input {text, wake: true, reason: "call"\|"heartbeat", calls, checkpoint}` |
| `ItemInput` external | P | none | none; A removes the id from `unobserved` |
| `ItemInput` control heartbeat | P | `system {text: Reason}` | `system {text: Reason, heartbeat: true}` (quiet) |
| `ItemInput` control settings or stop | P | none | none |
| `ItemTurn` | P | none | none (the Gate's `DeltaStart` drives `activity`) |
| `DeltaStart` | A | `activity {text: "model is thinking"}` | none |
| `DeltaText` / `DeltaThinking` (current Seq only) | P | `assistant-delta` / `thinking-delta` | none |
| `DeltaToolStart` | P | `activity {text: "writing <name> call"}` | none |
| `DeltaRetry` | P | `system {text: "provider hiccup — retrying…"}` | none |
| Seq or Attempt change with partial text shown | P | `delta-reset {seq}` **(new)** | none |
| `ItemModelResponse` Reasoning, non-empty Summary | P | `thinking` | `thinking {text: join(Summary, "\n\n")}` |
| `ItemModelResponse` assistant Message | P | `assistant` | `assistant {text, model, provider}` (+ `partial: true` when `Meta.Partial`) |
| `ItemModelResponse` with `Meta.Muted` | P | none | none |
| `ItemModelResponse` with `Meta.Err` | A | `error` | `error {text}`, then the §9.9 close |
| Stop `refused` | A | `error` | `error {text: "the model declined (<category>): …", refusal: category}` |
| Stop `max_output_tokens` | P | `system` | `system {text: "reply cut off at max_tokens; …"}` |
| first `ItemToolCallStatus`, ops, not `run_js` | P | `call {tool, id, phase: "start", text: detail, worker?}` | none (live only, as 5ea62f89) |
| first `ItemToolCallStatus`, `run_js` | P | `code {text: code}` | `code {text: code}` |
| first status with `Error` | P | `call {…}` | `call {text: detail, tool, id, ms: 0, error}`; `run_js`: `result {text, code, ms: 0, error}` |
| boughcall Progress | P | `call-delta {id, text, stream?}` **(new)** | none |
| terminal `ItemToolCallStatus`, not adopted | P | `call {…}` | `call {text: detail, tool, id, ms, exit?, add?, del?, job?, error?, canceled?, late?, output, truncated?, cmd?, worker?}` |
| terminal `ItemToolCallStatus`, `run_js` | P | `result`, plus `error` first when failed | `result {text, code, ms, exit?, error?}` |
| terminal status of an adopted call | A | `job` | `job {id, event: "finished", exit?, stopped?, call}` (jobs.go `finish`) |
| settle expiry | A | `job` per call | `job {id, event: "started", cmd: detail, call}` |
| close | A | `done` | `done {files, exit?, ms, after?, usage?{in, out, cache_read, cache_write, last_in, cost?, ttl?}, engine_turn, running?, wake?, stop?}` |
| cancel | A | `cancelled`, `done` | `cancelled {}`, then `done` |
| budget stop | A | `system`, `done` | `system {text}`, then `done {…, stop}` |
| Run error | A | `error`, `done` | `error {text: "engine: …"}`, then `done` if a turn was open |
| hooks fired | A | `hook` | `hook {event, name, ms, decision, error…}` from `Lifecycle.Drain()` (unchanged shape) |
| coordinator build, resume, fork, seed | A | none | `engine {engine: "unreal", pin: "v0.1.1", sha: "b7c9bf1c", session: sid, store, format: 2, system: sha256, system_file, provider, model, cache_ttl, tools: hash, forked_from?, fork_turn?, seeded?}` **(new, quiet)** |
| ask tool | ask row | `ask {ID, Options, Secret}` (ask.Event) | `ask {question, options, id, secret?}`, `ask/answer {id, text}` (unchanged) |
| todo tool | todo row | `todo` | `todo/add`, `todo/done`, `todo/clear` (unchanged) |
| child session items | P (prefix `sub:`) | `sub:start {text, worker}`, `sub:assistant`, `sub:call` (live start + recorded end), `sub:error`, `sub:done {worker, status, steps}` | same kinds and keys as workers.go:436-556 today |

**Call entry keys:**

| Key | Value |
|---|---|
| `text` | the detail |
| `ms` | the terminal status's `RecordedAt` minus the first status's |
| `exit`, `add`, `del`, `job`, `cmd` | from `Handle.Data`; `cmd` is the full bash command, ≤2 KiB, for serve's test matching |
| `output` | the `Render` text, head+tail within `row_output` |
| `truncated` | set when `output` or the model text was cut |
| `late` | set when an `ItemTurn` happened while the call was running, so the model saw the placeholder |
| `canceled` | set when the op ended canceled |
| `error` | the failure text, with the `Error: ` prefix trimmed |

A failed native call is a recorded `call` with `error`. It is never an
`error` event or entry, so headless `hlTurnErr` (headless.go:222-242)
is set only by turn-level failures, and a recovered tool failure does
not exit 1.

**The ordering rules below carry load:**
- `cancelled` is immediately followed by `done`.
- A steer never produces a `done`.
- `input {wake}` comes before any entry of its turn.
- An adopted call's `job finished` lands where it happens, between
  turns. `job` is QUIET in the web, and the TUI job strip reads
  `Running()`.

### 10.2 Kinds that change

- **New:**
  - `call-delta` (live): `{id, text, stream?, worker?}`;
  - `delta-reset` (live): `{seq}`;
  - `engine` (recorded, quiet).
- **Gain keys:**
  - `call`: `output`, `truncated`, `late`, `canceled`, `cmd`, `job`;
  - `done`: `engine_turn`, `running`, `wake`, `stop`, `usage.ttl`;
  - `input`: `input_id`, `wake`, `reason`, `calls`;
  - `job`: `call`;
  - `error`: `refusal`.
- **`call` becomes the primary tool row** on the engine. `code` and
  `result` are written only for `run_js`.
- **`nudge`** is written only for stop-hook continuations and schema
  misses.
- **Unchanged:** assistant, thinking, ask, ask/answer, steer,
  cancelled, system, hook, todo/*, title, turn-summary, undo, model,
  notice, notice-delivered, meta, origin, sub:*.

### 10.3 hseq catch-up and `bough engine` (W2 + W4)

At `Open`, the Runtime reads the highest `data.hseq` in the history
file. It then projects `store.Items(sid, after: max)` in **catch-up
mode**:
- it records content rows only (assistant, thinking, call, error,
  system);
- it records no turn structure, because `history.go:816` already
  closed the open turn with `cancelled {interrupted: true}`;
- it emits no events.

A kill -9 between a store append and a history write therefore loses
nothing and duplicates nothing.

The CLI, in `cmd/bough/enginecmd.go`:
- `bough engine inspect <sid|history-id>` prints the store items, one
  line each.
- `bough engine reproject <id> [--dry-run]` re-runs the whole store
  through the projector and prints (or appends) the missing rows.
- `bough engine script <id> [-o file]` writes an `llm-script` JSON
  from the session's ModelResponses (`fake.FromStore`). It is the
  replacement for `plugins/replay` and the way to turn a real session
  into an e2e fixture.

### 10.4 Reader changes (W4, all additive; loop sessions and old files still render)

**serve** (`internal/serve`):
- `supervisor.go:758` inTurn: add `call`, `call-delta`.
- `deltas.go:30` isDelta: add `call-delta`, coalesced by `data.id`.
- `turns.go:39-60` turnTests and `signals.go:147-170` lastTest: also
  read `call {tool: "bash", cmd, exit}`.
- `signals.go:180-196` LastCache: use `done.usage.ttl` when present.
- `status.go:94`: add a test that `ask/answer` clears NeedsYou for an
  engine session.
- `api.go:684-705` lastModel: also read the latest `engine.model`.
- `changes.go`: unchanged (`done.files`, `after`, and `input.checkpoint`
  are the same keys).

**web** (`internal/serve/web/src`):
- `render.tsx:350` QUIET: add `engine`.
- `render.tsx:371-410` groupTurns: `input {wake}` opens a turn rendered
  with a quiet "background call finished" header instead of a user
  bubble.
- `render.tsx:455` groupTools gate: `code || call`.
- `render.tsx:689` splitWork: count `call`.
- `app.tsx:1966-1978` walker: group a run of `call` rows with no
  `code` parent.
- `app.tsx:2023` runSummary: read `call.data.cmd` or text.
- `app.tsx:2402` callEdits: read `call` `add`/`del` and path.
- `app.tsx:2778`, `2824`: code-kind lists include `call`.
- `app.tsx:2855-2861` fail pointer: the last `call` with `exit ≠ 0` or
  `error`.
- CallRows / CallMeta get an expandable body from `data.output` and
  `late`/`canceled` badges.
- RunningCall shows the latest `call-delta` tail.
- `delta-reset` clears streamed text for that seq.
- A turn footer with `done.running` shows "N calls still running",
  linking to Work.
- The effort picker gains `max`.
- CSS goes in `dist/index.html`, then `bun run design:sync`. Rebuild
  `dist/app.js`.

**TUI and headless** (`plugins/ui`):
- `model.go:1199-1202`: replace "ignore call" with a call-row
  renderer. A start opens a running row with a spinner and a 3-line
  `call-delta` tail. The end collapses to one line:
  `detail · ms · exit N | +add −del`, red on error, `(bg)` when
  adopted. `sub:call` is the same, indented under its worker.
- `model.go:1131` gotOutput: also set by `call` and `activity`.
- `delta-reset` drops the live buffer, and so does `landSteer`.
- `input {wake}` renders a dim "↻ background call finished" line.
- `session.go:80-112` resume replay: skip `engine`, render `call`
  rows.
- `headless.go:227` hlPrint: forward `Data` for `call-delta` and
  `delta-reset` under `--json`.
- `headless.go:255`: a `done` with `Data["wake"] == true` does not
  decrement `hlPending`.
- `headless.go` at EOF: call `drain` (§9.10).
- teatest goldens regenerated with `-update`, never hand-edited.

**title / activity / cmux:**
- `title/turns.go:60-110` builds gists from `call` (and `code`), and
  exits from `call.exit`.
- `activity.go:126-131` treats a `call` start like `code`.
- `cmux.go:180` adds `call`.

All three keep asserting `loop.Event`.

## 11. Context, skills, hooks, rules, MCP (W5)

### 11.1 Frozen system prompt

`prompt.Compose(Parts)` runs at each coordinator build. It
concatenates the parts in this order:
1. `Preamble`: `internal/unreal/prompt/engine.md`, or the row's
   `system_prompt`.
2. `Env`: cwd, platform, and the date at build.
3. `Guidance`: `task_guidance`.
4. `Ask`: the ask section, when `ask-answers` is mounted.
5. `SessionStart`: session-start hook context, first build only.
6. `Context`: context-md `Parts()`, i.e. AGENTS.md, CLAUDE.md,
   MEMORY.md, deduped by section as today.
7. `Sections`: `prompt-sections`, sorted by name.
8. `Skills`: the skills catalogue (name, description, path).
9. `Schema`: `SchemaSection`, when stop-schema is set.

The result is written once, to `<sid>.system.md`, and its sha goes on
the `engine` entry. Every later build, resume or model switch reuses
that file byte for byte. Any difference between it and the current
parts goes out as a reminder (§11.2), so the system text never changes
within a session. A tool-set change still changes the `tools` block
(§19 risk 2).

**engine.md** (W5 writes it; no harness branding) says:
- Calls run asynchronously. A call still running after about a second
  shows a placeholder.
- You can keep working on something independent, or end your reply to
  wait. You are woken when a call finishes.
- Ending a reply with nothing running hands control back to the user.
- A result that arrives after you moved on appears in a user message
  as `<tool_result call_id=… name=…>` text. It is that call's output,
  not the user speaking. Never re-run a call that is still pending.
- Use `bash` with `background: true` for servers and watchers.
- Make parallel calls when they are independent.
- A `<context-update>` block in a user message is an update to your
  instructions from the harness, not from the user.
- The scope-and-verification contract from f9ee4d36 carries over, in
  its non-code-mode parts.
- A heartbeat line is included only when `heartbeat > 0`.

### 11.2 Drift reminders

At every Submit and notice wake, the actor calls
`prompt.Reminder(lastParts, Deps.Prompt())`. `lastParts` starts as the
parts the system file was composed from, and is persisted as
`~/.bough/engine/<sid>.parts.json` after every reminder, so a resumed
session diffs against what the model actually last saw. A non-empty result is
prepended to that input's payload as:

```
<context-update source="AGENTS.md, prompt-section orb">
…the changed parts, whole…
</context-update>
```

It emits one `context` event per changed piece. Then `lastParts`
becomes the current parts for the next diff. Input[0] is never edited.
The reminder is persisted with the input, is append-only, costs no
extra wake, and replays byte-identically. The same mechanism carries
the orb-ready note and a MEMORY.md written mid-session.

`/context` shows:
- the frozen prompt file and its sha;
- pending reminders;
- the tool list with its hash;
- the store path;
- the pin;
- the adapter's provider and model.

### 11.3 Skills

bough's mechanism is kept and the harness's `skill_use` is not used.
That avoids a restore coupling to SkillUse and to the harness's
single-line frontmatter parser.
- The catalogue goes in the frozen prompt (§11.1 part 8).
- Mention-triggered `skills.Inject` runs at Submit.
- The `/name` commands are unchanged.
- The model reads a SKILL.md it wants with `view`.
- Rewrite the two `tools.*` references in
  `go/skills/multi-model-plan/SKILL.md`, so that they read under
  either engine.

### 11.4 Hooks: `internal/unreal/hookbridge`

```go
type Firer interface {
	Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error)
	TakeFireRecords() []map[string]any
}
func New(get func() (Firer, bool)) *Bridge // implements agenttools.Hooks and session.Lifecycle
```

| Engine point | Hook event | Payload | Honoured result keys |
|---|---|---|---|
| first coordinator build | `session-start` | as the loop sends | context text → `Parts.SessionStart` |
| Submit | `user-prompt-submit` | `{input}` | the loop's block and rewrite keys (loop.go:839-898) |
| boughcall before `Tool.Call` | `pre-code-exec` | `{code: detail, tool, args, call}` | the loop's deny key → deny; `args` (object) → replace the arguments |
| boughcall after `Tool.Call` | `post-result` | `{code: detail, tool, call, result: text, error}` | `result` → rewrite Text |
| close (§9.3) | `stop` | `{reply}` | the loop's block reason → `continueWith` |
| unmount | `session-end` | unchanged (hooks.go:462) | none |

Rules for the hook bridge:
- `code` carries the detail (bash's command line), so existing hooks
  that match command text keep matching.
- A `Fire` error is reported as a hook record, never as a fatal error,
  and the partial result still applies. That is the loop's rule
  (loop.go:1252-1308).
- Records drain into `hook` entries.
- Hooks run on the shared codemode VM via `RunHook`, as today. The
  codemode row stays mounted with the engine.
- Hook calls into `tools.*` stay suppressed on the host
  (`hookmeta.OnHost`).
- No hook runs inside a translator.

W5 also updates `go/docs` hook docs for the new payload keys.

### 11.5 Rules

`prefix_rule` forbid/prompt/allow and path-scoped notes already run
inside `bashRun`, `writeFile` and `patchFile` through turn-stats policy
(rules.go:368-389). Native calls reach them unchanged. W5 adds engine
tests for:
- a forbidden prefix returning an error to the model;
- a `prompt` rule going through `Asker.Ask`;
- a path-scoped note appearing on a native `write`.

Unscoped markdown rules reach the frozen prompt through their prompt
section.

### 11.6 MCP

The default is unchanged: CLI over the shell (`bough mcp call …`)
through native `bash`, with the prompt section in the frozen prompt.

With `mcp` row config `native_tools: [server, …]`, the row registers
one `agenttools.Tool` per catalogued tool of each listed server:
- The name is `mcp__<server>__<tool>`. Every byte outside
  `[a-zA-Z0-9_-]` becomes `_`, the name is truncated to 64 bytes, and
  a clash gets a `_2` suffix.
- The schema is the server's input schema.
- The call goes through the existing sdk client (`connect` + a
  `CallTool` with the raw args object). It reuses the connection
  within the session, and the result's text content is the Result.

Native MCP tools change the tool set when servers connect or
disconnect, which restarts the coordinator at idle (§2). That is why
the list is opt-in and never on by default.

## 12. Resume, fork, subagents, background agents (W2)

### 12.1 Resume

`-r`, serve's respawn and `/sessions` all go through `Open`. `Open`
reads the latest `engine` entry to get `sid`, runs `store.Resume` to
validate, and runs catch-up (§10.3). **The coordinator is built at the
next input, never at Open.** An interrupted turn therefore never
triggers a model call the user did not ask for. When the new
coordinator starts, it answers the new input together with any
unanswered earlier inputs and finished calls, which is harness
behaviour. Non-terminal ops from the previous process are re-Added and
fail as interrupted (§2).

### 12.2 Seed

Seeding applies to a history with no `engine` entry (a loop session),
and to a Resume that fails (a missing file, or a format bump). The
actor creates the harness session and prefixes the first input's
payload with a transcript:
- the transcript is `loop.DefaultProject(entries)`, rendered as
  role-labelled text inside `<earlier-session>…</earlier-session>`;
- it is capped at the last 400 KiB, with a note saying so;
- the actor records `engine {…, seeded: "loop"|"reseeded"}`.

The seed is lossy by design: bough history is the truth, and the
harness store can be rebuilt. Resuming an engine session on the loop
also works, because DefaultProject reads `input`, `assistant` and
`nudge`. W2 extends it to fold `call` rows into the result text the
way `result` rows are folded; that change is additive in
`plugins/loop`, owned by W2.

### 12.3 Fork

`history.Fork` (plugins/history/tree.go:380) copies a finished turn
(input…done), including the parent's `engine` entry. On the child's
first build the engine does one of two things:
- If the fork point's `done.running > 0`, it **refuses**, with an
  error naming the calls:
  `engine-unreal: cannot fork at a turn whose calls were still running (job 3: go test ./...); fork from the turn before`.
  The harness Fork leaves inherited running calls without results (its
  FIXME).
- Otherwise it calls
  `store.Fork(childSid, parentSid, done.engine_turn)` and records
  `engine {…, forked_from: parentSid, fork_turn}`.

The harness fork also strips the operations off every inherited call
status, and a coordinator replaying a status without them adds no
result, so every earlier call would reach the child's model without an
answer. Every coordinator of a forked session therefore reads its
history through `forkedStore` (`session/fork.go`), which puts back the
operations from the parent's file (and its parent's, for a fork of a
fork) by sequence, which a fork keeps. The replay renders each result
as the parent did, and the fork item that follows clears them.

### 12.4 Subagents (`session/children.go`)

`Children.Run` builds a child coordinator. The child has:
- its own `localfile.Store` instance (file `<sid>-w<n>`, same dir);
- its own `ops.Manager` + `LocalOperationManager` + boughcall handler
  (`Worker` set);
- its own Gate;
- an adapter from the same Source with `Options{Worker}` and no Sink;
- a projector with `Prefix: "sub:"`, `Worker`.

Tools are `Allow`, which defaults to the parent's tools minus `spawn`,
`agent`, `stop_agent`, `ask` and `secret` (depth 1, as workers today).
The system prompt is the parent's frozen prompt with
`TextExcept("workers")` plus `System`. The task is submitted as an
InputExternal, followed by StopWhenIdle.

The child ends in one of four ways:
- **done:** Run returns after StopWhenIdle. The reply is the last
  assistant text, and the child records
  `sub:done {worker, status: done, steps}`.
- **budget:** at `MaxSteps` requests the Gate mutes and the child is
  stopped with StopHard. Status `budget`.
- **cancel:** the ctx is cancelled and the child is stopped with
  StopHard. Status `cancelled`.
- **error:** status `error`, and the reply carries the error.

A child's calls are the parent's foreground `spawn` call, so parallel
`spawn` calls fan out.

### 12.5 Background agents

`spawn {background: true}`, `agent` and `stop_agent` are unchanged:
they call `serveclient.CreateChild`. Finish notices arrive as
`{"notice":…}` lines, go to job-notices and wake the parent (§9.4).

## 13. Code mode

| Option | For | Against |
|---|---|---|
| A: native only | Matches the harness preamble and its benchmarks (TB4 57.9%). Parallel fan-out is where code mode loses about 14 points. | Loses 12+-step chains (about +18.8 for code mode) and makes the recorded code-block corpus unreplayable. |
| **B: native, plus opt-in `run_js`** | Keeps chains and the corpus. Costs little: goja stays anyway for serve watchers (watchers.go:48), hook dry-run (hooks.go:520), hooks and init.js. | Two tool surfaces, and parallel `run_js` calls serialize on one VM. |
| C: `run_js` only | none | Throws away the harness's async fan-out, its reason to exist. |

**Chosen: B.** Native is the engine default (`tools: native`), and
`tools: both` adds `run_js`. The loop, which is pure code mode, stays
the product default, which honours the 2026-09-22 KEEP verdict.

The flip needs a same-task bench:
- **Arms:** loop, engine `native`, engine `both`.
- **Models:** claude-sonnet-5 and one GPT model.
- **Seeds:** k=3, with k=2 as the noise band.
- **Metrics:** solved rate, $/solved, wall time.
- **Harness:** `bench/harbor/bough_go_agent.py` with an engine-rows
  variant (W6).
- **Decision:** the user's, on those numbers.

## 14. Deterministic adapters

### 14.1 `internal/unreal/fake` (B0 core; W1 owns afterwards)

```go
type Step struct {
	Match  func(ullm.Request) error // nil = accept; an error fails the test with the rendered request
	Want   string                   // JSON form: substring of the last user-side text
	Output []ullm.Item
	Stop   ullm.StopReason          // "" = complete
	Usage  ullm.Usage               // zero = {InputTokens: 100*len(Input), OutputTokens: 10}
	Deltas []agentllm.Delta         // sent through Options.Sink, Seq from ctx, before returning
	Hold   <-chan struct{}          // block until closed or ctx done (steer/cancel tests)
	HoldMS int                      // JSON form of Hold
	Err    error                    // returned from Respond (the Gate converts it)
}
// Reporter is satisfied by testing.TB; the package does not import "testing",
// because llm-script links it into the binary.
type Reporter interface{ Helper(); Errorf(format string, args ...any) }
func New(r Reporter, steps ...Step) *Adapter // r == nil (llm-script): a mismatch answers "[script: …]" text
func (a *Adapter) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error)
func (a *Adapter) Provider() string // "fake"
func (a *Adapter) Model() string    // "fake-model"
func (a *Adapter) Close() error
func (a *Adapter) SetOptions(o agentllm.Options)
func (a *Adapter) Requests() []Recorded // deep copies of every request and its options
func (a *Adapter) Wait(ctx context.Context, n int) error // until n requests were made
func Text(s string) ullm.Item
func Call(id, name, argsJSON string) ullm.Item
func Think(summary string) ullm.Item // Raw {"type":"thinking","thinking":summary,"signature":"fake"}
func Load(path string) ([]Step, error)
func FromStore(dir, sid string) ([]Step, error)
func AssertAppendOnly(r Reporter, reqs []Recorded)
type Recorded struct{ Request ullm.Request; Options ullm.RequestOptions }
```

The contract:
- Steps are consumed FIFO. Nothing touches the network or a clock
  except `Hold`/`HoldMS`.
- A superseded or cancelled request sees `ctx.Err()`.
- An exhausted script calls `r.Errorf` and answers
  `"[script exhausted]"`; it never hangs.
- `llm-script` steps with `text` also stream it word by word as
  `DeltaText`, like echo, so the UIs see live deltas.
- Call ids come from the script. When they are omitted, they are
  `call_<step>_<n>`, stable across a Resume.

JSON form (`llm-script`). The step keys are `want`, `text`, `think`,
`calls` (`[{id, name, args}]`), `stop`, `usage`
(`{input, cached, cache_write, output}`), `hold_ms` and `error`:

```json
{"steps": [
  {"want": "CODE!", "think": "plan", "calls": [{"id": "c1", "name": "bash", "args": {"command": "echo hi"}}]},
  {"text": "ran it", "usage": {"input": 1200, "cached": 1000, "output": 40}},
  {"want": "slow", "hold_ms": 3000, "text": "done waiting"},
  {"error": "overloaded"}
]}
```

`AssertAppendOnly` checks two things. First, every request's messages,
except its last user-side run, are a prefix of the next request's
messages. Second, every ToolCall id has exactly one first result in the
user-side run that follows it.

### 14.2 `internal/unreal/echo` (W1)

`echo` is stateless and deterministic. It streams word by word through
the Sink, which exercises the live UI with no network. Its rules, in
this order:

| Condition | Response |
|---|---|
| the last user text contains `SYSTEM!` | assistant text = `Input[0]` text |
| the last item is a ToolResult (a result is the newest input) | assistant `ran: <first line of the result>` |
| the last user text contains `CODE!` | `ToolCall bash {"command":"echo hi from codemode"}`, id `echo_<len(Input)>` |
| the last user text contains `SLOW!` | `bash {"command":"sleep 3; echo slow done"}` |
| the last user text contains `ASK!` | `ask {"question":"echo asks?","options":["yes","no"]}` |
| the last user text contains `SPAWN!` | `spawn {"task":"say hi"}` |
| otherwise | assistant `echo: <last user text>` |

The AGENTS.md smoke must pass on both engines:

```sh
printf 'say CODE! please\n' | go run ./cmd/bough --headless --set llm.plugin=llm-echo --set loop.plugin=engine-unreal
```

## 15. Test plan

All Go tests follow the house rules:
- `t.Parallel()`;
- a `t.TempDir()` HOME (set `HOME`, and `BOUGH_SCRATCH` under it);
- a deterministic adapter (`fake`, `echo`, `llm-script`);
- no network, and never the real `~/.bough`.

Judge a run with `grep -E "^(FAIL|--- FAIL)"`, never with `tail`.

### 15.1 Per workstream

**W1**
- **messagesapi Render goldens:**
  - a placeholder then a late result;
  - two parallel calls, one late;
  - a synthesized missing result;
  - foreign reasoning dropped;
  - invalid args;
  - png and jpeg images;
  - an unsupported image mime;
  - the B1–B3 markers;
  - a merged user run after a muted response.
- **The rapid property test**, which is the stop-the-line gate. It
  drives the real `contextbuilder.NewBuilder()` through `prompt.Wrap`
  (a local copy of Wrap's two substitutions until W5 lands) with random
  `AddModelResponse` (text, calls, reasoning), `AddToolResult(running)`,
  final `AddToolResult`, `AddExternalInput` and `Commit`. With
  `cache_control` stripped, it asserts:
  - request k's messages except the last are byte-identical in
    k+1;
  - k's last message's blocks prefix the same message in k+1;
  - every `tool_use` has exactly one `tool_result` in the next
    message;
  - no render ends on assistant.
- **Decode** from recorded SSE fixtures in `testdata/*.sse`: signed
  thinking, redacted, tool_use, a tool_use truncated by max_tokens, a
  refusal before output and mid-stream, a fallback block, updates
  display.
- **Retries** against `httptest`: 429 with retry-after, 529, an
  in-stream `overloaded_error` with status 200, EOF mid-stream, a 400
  that is not retried, beta self-heal, the signature strip, overflow
  typed, the idle watchdog.
- **responses:** tap goldens from recorded OpenAI and OpenRouter SSE
  (text, reasoning summary, function_call added, Seq/Attempt), plus the
  request-body parity test against the harness clients.
- **wrap:** envelope round trip and foreign drop across
  anthropic→openai→anthropic; LateResultsAsText goldens; the strip
  self-heal.
- **fake / echo / script:** step matching, `Hold`, exhaustion, JSON
  load, echo rules.
- **Rows:** `AgentAdapter` on each llm row; usage tally inclusive;
  1h pricing; `max` effort; the cachettl fix; the retry.go `Type()`
  fix with a test.

**W2**
- **ops:** re-Add replays Latest; a queued update coalesces with the
  replay; Updates survives coordinator restarts; the manager ctx ends →
  channel closed.
- **session end-to-end** with `fake` + a real localfile store + a real
  `LocalOperationManager` + testkit tools (echo, fail, hold-until-chan,
  emit-progress). Until W3 lands, the registry comes from the harness
  `tool.NewRegistry` with real `bash`. Scenarios:
  - a plain reply;
  - a call inside the grace window;
  - a slow call: placeholder, the model ends its reply, no `done`
    until completion, then `done`;
  - settle expiry → `job started` + `done{running}` → completion → a
    wake turn with `done{wake}`;
  - a queued steer during an in-flight request;
  - a steer with `steer_interrupts` (delta-reset, no extra done);
  - cancel during a request and during a call: parked, no provider
    request, then the next input reaches the provider;
  - a background notice wake;
  - an ask round trip;
  - a provider error → `error` + `done`, with Run alive;
  - sticky overflow;
  - refusal; truncation;
  - `/model` mid-session: a new adapter, no restart, foreign reasoning
    dropped;
  - a tool-set change → restart at idle;
  - process restart → the awaiting op fails as interrupted;
  - fork, and the fork refusal;
  - a subagent;
  - seed from a loop history;
  - hseq catch-up after a simulated crash;
  - max_steps; Run death → rebuild.
- **The §9.11 property test.**
- **plugins/engine:** mounts with history and agent-tools; provides
  the seven keys with the loop's types (a compile-time assertion against
  what ui reads); remount handoff; `--set loop.plugin=engine-unreal`;
  `/context`; stored notices.

**W3**
- agenttools registry: register, clash, invalid name, unregister,
  Changed.
- toolreg: Resolve is always true; args validation; `Render` is pure
  (the same op gives the same bytes); a tombstone restores a real store
  holding a retired tool.
- boughcall:
  - every legal transition, accepted by a real `LocalOperationManager`;
  - timeout, cancel;
  - re-Add of awaiting → interrupted;
  - Progress;
  - spill file;
  - hook deny and rewrite;
  - redaction before the state (a secret never appears in any
    `Updates()` snapshot).
- Each native tool against its row, including the write-roots rule (no
  roots → no write/patch) and project mode refusing without an orb
  (fake `orb` service whose Command fails).
- `Jobs.Adopt`: numbering shared with background bash, `/jobkill` of
  an adopted job cancels the op, `Running()`.

**W4**
- project goldens: JSON of Outs for item sequences, regenerated with
  `-update`, never hand-edited, one per row of §10.1.
- serve unit tests: inTurn on `call`; delta runs keyed by id;
  turnTests and lastTest from `call` rows; LastCache ttl; lastModel from
  `engine`.
- ui teatest goldens: call rows, call-delta tail, delta-reset, wake
  line, adopted row. Headless `--json` forwards call-delta; a wake
  `done` does not decrement; a failed native call followed by success
  exits 0.
- web: `bun test` for groupTools, splitWork, the walker, the fail
  pointer and delta-reset; `bun run typecheck`; one Playwright spec
  driving a real `bough --web` on `llm-script` + `engine-unreal`
  (running row → final row, wake header, expandable output).

**W5**
- prompt: Compose determinism; Reminder diffs; Wrap substitutions;
  append-only under random operations.
- hookbridge: each event's payload and honoured keys; Fire errors are
  non-fatal.
- skills/context-md/rules/mcp engine tests (§11.3–11.6).

**W6**
- `go/e2e`: headless subprocess tests on `llm-script` +
  `--set loop.plugin=engine-unreal`, for these scenarios:
  - text;
  - tool call;
  - async fan-out (3 parallel calls);
  - in-progress → final (`hold_ms` + a late result);
  - ask (answered on stdin);
  - cancel (SIGINT → exit 130 → `-r` resume);
  - steer;
  - resume;
  - fork (`/tree`);
  - background agent (serveclient stub);
  - the AGENTS.md echo smoke.
- `internal/vtreal`: one PTY scenario on engine + echo (CODE!, a call
  row visible, done). No existing vtreal file changes.
- pin and import-boundary tests (§16).

### 15.2 Contract tests against the pin (`internal/unreal/contract`, W2)

Each test pins a harness behaviour bough relies on that the README
does not promise. A pin bump that breaks one is a design review, not a
test fix.
1. Completions within 1s of a response are batched into one request.
   A completion inside grace plus a call still running at expiry gives
   exactly one request.
2. A response with no tool calls and nothing pending leaves the
   coordinator idle: no request until an input or a completion.
3. An empty completed Response is persisted and treated as idle.
4. `LocalOperationManager.Add` of a known ID returns nil and emits
   nothing (the reason for §5.5).
5. Cancel Run's ctx between `Updates()` and `SaveOperation`, then build
   a new coordinator on the same `ops.Manager`: the call reaches a
   terminal status.
6. Observers fire synchronously, in order, only for appended items.
7. `Response.ID` is never read by the coordinator.
8. `Resolve` is called for every historical call at restore; a
   tombstone satisfies it.
9. A superseded request's ctx is cancelled and its response is not
   persisted.
10. `store.Fork` at a turn boundary with no running calls resumes and
    runs.
11. The restore of `testdata/stores/v0.1.1/*.session.jsonl` (recorded
    by bough's own code at this pin) produces byte-identical golden
    requests for both renders.

### 15.3 Live probes and smoke (manual; never in `go test ./...`)

`scripts/unreal-smoke.sh` is W6's. It is guarded per provider by
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY` and `OPENROUTER_API_KEY`: a
missing key skips that provider with a line saying so. It never echoes
a key, and runs with `set +x`. It builds `./cmd/bough` into the
scratch dir and uses a temp HOME seeded only with `bough.yml` rows.
For each of these providers:
- `llm-anthropic` with `claude-sonnet-5` and `claude-opus-5`;
- `llm-openai` with a GPT-5.x;
- `llm-openrouter` with one `openai/*` and one `anthropic/*`;

it runs, headless `--json` on `engine-unreal`:
1. a plain reply;
2. two parallel calls;
3. `sleep 5; echo late` followed by a question (placeholder and late
   result);
4. cancel during a call, then a next message;
5. kill and `-r` resume;
6. `/think high`;
7. a provider switch mid-session.

Each run asserts:
- exit 0;
- the number of dones equals the number of lines;
- no 400s in the trace;
- Anthropic `cache_read` growing from the second request.

**Probes** (W1 runs them and records the results in the
`internal/messagesapi` package doc):

| Probe | What it checks | Expected / decides |
|---|---|---|
| P1 | Opus 5.5 with a committed placeholder then a late result, under `Binding: "error"` | no 400, empty `input_transformations`, `cache_read` growing |
| P2 | P1 with `cache-diagnosis-2026-04-07` | no divergence |
| P3 | the longest SSE byte gap during max-effort thinking | sizes `IdleTimeout` |
| P4 | what `display: "updates"` returns on Opus 5.5 and Fable 5.1 | |
| P5 | OpenRouter + `anthropic/*`: a duplicate `function_call_output` | 400, merge or drop; decides `late_results` |
| P6 | OpenRouter + `anthropic/*`: thinking signature round trip through Responses | decides reasoning replay for that family |
| P7 | a historical `tool_use` for a tool absent from `tools`, on both APIs | accepted, or tombstoned names must stay listed |

## 16. Dependency hygiene

- **go.mod** gets
  `github.com/unreallabsai/unreal-agent v0.1.1 // b7c9bf1c5c2fa4127255c07727a7c8413e23944a`,
  written by `go get github.com/unreallabsai/unreal-agent@b7c9bf1c5c2fa4127255c07727a7c8413e23944a`
  (run in `go/`).
- MVS bumps `golang.org/x/sys` v0.47.0 → v0.48.0 and adds
  `github.com/oapi-codegen/runtime v1.6.0`, `golang.org/x/image v0.46.0`
  and `github.com/apapsch/go-jsonmerge/v2 v2.0.0` (indirect);
  `google/uuid` is already present. All are pure Go, with no cgo and no
  GOEXPERIMENT; stdlib `uuid` and `encoding/json/v2` are baseline in
  go1.27.
- B0 records `go build -ldflags='-s -w' ./cmd/bough` size before and
  after in its commit body.
- **No vendoring** (bough does not vendor); go.sum and GOSUMDB are the
  integrity check. A local-path `replace` is never committed. A needed
  patch goes upstream as a PR first. If that is blocked, use a
  published fork via `replace`, with the reason in a go.mod comment and
  a test that fails once upstream has the fix.
- **Pin test** (`internal/unreal/pin_test.go`, W6):
  `debug.ReadBuildInfo` in a test binary asserts the module version is
  `v0.1.1`. `internal/unreal.Pin = "v0.1.1"` and
  `PinSHA = "b7c9bf1c…"` are the constants the `engine` entry writes.
- **Import boundary** (`internal/unreal/boundary_test.go`, W6):
  `go list -f '{{.ImportPath}} {{join .Imports " "}}' ./...`, which lists
  direct imports. Only these may import
  `github.com/unreallabsai/unreal-agent/...` or `internal/unreal/...`
  directly:
  - `internal/unreal/...`;
  - `internal/messagesapi`;
  - `internal/agentllm`;
  - `plugins/engine`;
  - `plugins/llm`;
  - `cmd/bough`.
- **Imported harness packages** are public only: `harness/{coordinator,
  inbox, session, sessionstore, sessionstore/localfile, contextbuilder,
  llm, llm/responsesapi, tool, tool/viewimage, operation, primitives}`.
  Not used: `tool.NewRegistry` (except in W2's interim tests), the
  Shell op, SkillUse, the embedded preamble, `cmd/internal/agentrunner`.
- **Upgrade checklist** (one PR, subject
  `engine: bump unreal-agent to <tag>`):
  1. Read upstream's diff of coordinator, sessionstore, contextbuilder,
     operation, tool, inbox, llm/responsesapi and primitives.
  2. `go get …@<sha>`.
  3. Run the contract tests, the compat corpus, the projector goldens,
     the messagesapi property test, and the live smoke.
  4. If the localfile format changed (2 today), either write
     `bough engine migrate` or explicitly accept the lossy reseed
     (§12.2). The user decides, per bump; it never happens silently.
  5. Add the new pin's store recordings under `testdata/stores/<pin>/`.

## 17. Removal list, in dependency order (none of it lands in this run)

Every item needs the default flip plus the user's explicit yes, and is
its own revertible diff. A package is deleted only after its
replacement carries the coverage its tests had.

| # | Remove | Replaced by / precondition |
|---|---|---|
| R1 | `bench/evolve` (tunes the loop's js-fence prompt); retarget `bench/harbor/bough_go_agent.py` lines 50-85 to engine rows | the W6 engine variant of the harbor agent |
| R2 | `plugins/replay` | `llm-script` + `bough engine script`; re-point the 32 vtreal replay files |
| R3 | init-js `llm`, `cognition`, `projection` surfaces and keys (theme, keymap, commands and notice stay) | nothing reads them once the loop runner is gone |
| R4 | the workers codemode child mini-loop (users of `loop.Finish`, `StripFabrications`, `MaxBlocks` in workers.go:428-556) | the `engine` key's `Spawn` (§12.4); `spawnAll` stays while `run_js` exists |
| R5 | the `plugins/loop` runner half: Run, block extraction, vetoes/nudges, receipts (737104cd), the prompt contract (f9ee4d36), trim.go, cancel.go turns and handoff, completeMsgs/retries. First move `Event`, `Sections`, `SumUsage`, `UsageDelta`, `ExpandAt`, `StopAnswer`, `DefaultProject` to `internal/agentevent`, `internal/promptsections`, `internal/turnkit`, with aliases. | every importer repointed (activity, cmux, cost, initjs, title, tools, workers, cmd/bough, tests) |
| R6 | loop-only provider paths: `StreamThinking` (if unread), `cerebras.go` (unless kept for llm-small) | `Complete`/`Stream` stay for llm-small |
| R7 | `internal/uitest` JS()/Bash() fence stubs; the 77 js-asserting vtreal files, each rewritten or deleted with a named reason | fake-adapter stubs |
| R8 | docs: the README thesis, AGENTS.md line 1, go/README.md, docs/PLUGINS.md, background-agents.md, orbs.md, secrets.md, INIT.md; re-record scripts/demo | the flip |

**Never removed:**
- the codemode package and row, and goja (hooks via RunHook, serve
  watchers and hook dry-run, `run_js`, init.js);
- `tools/jobs.go` and `tools/calls.go`;
- `ui/blocks.go`;
- every `code`/`result` reader in serve, web and the TUI, for old
  sessions.

## 18. Workstreams and file ownership

### 18.1 B0: bootstrap (serial, before any fan-out; one agent, W6's)

1. In `go/`, run
   `go get github.com/unreallabsai/unreal-agent@b7c9bf1c5c2fa4127255c07727a7c8413e23944a`
   and then `go mod tidy` after step 4. Record the binary size before
   and after.
2. Write §4.1 (`internal/agenttools`, with `NewRegistry` and
   `ValidName` implemented) and §4.2 (`internal/agentllm`) verbatim.
3. Write `plugins/agenttools`, which is complete in B0 (Apply provides
   `agenttools.NewRegistry()` under `agent-tools`).
4. Write a `stub.go` for every other package in §3, with the §5
   signatures and bodies returning `errNotYet("<pkg>.<Func>")` or zero
   values. `internal/unreal/fake` is written complete, per §14.1, minus
   `Load` and `FromStore`. `plugins/engine` registers `engine-unreal`,
   whose Apply returns
   `fmt.Errorf("engine-unreal: not built yet (see go/docs/unreal-engine.md)")`.
5. Write `internal/unreal/doc.go` with `const Pin = "v0.1.1"` and
   `const PinSHA = "b7c9bf1c5c2fa4127255c07727a7c8413e23944a"`. In
   `cmd/bough/main.go`, add the blank imports for `plugins/engine` and
   `plugins/agenttools`. In `go/bough.yml`, add the `agent-tools`
   row after `commands`.
6. Run the gates:
   - `gofmt -l` on the touched files;
   - `go vet ./...`;
   - `go build ./...`;
   - `go test -race -parallel 4 ./internal/agenttools/... ./internal/unreal/... ./plugins/agenttools/...`.

   Commit as
   `engine: pin unreal-agent and stub the engine packages`.

Every workstream branches from B0. After B0, `go/go.mod` and
`go/go.sum` belong to W1 alone.

**B0 as it actually ran.** B0 did not run before the fan-out: the base
branch held only this document. W1 did step 1 and §4.2 (the pin and
`internal/agentllm`). W6 did steps 3 and 5 and the parts of 2 and 4
that its own wiring compiles against, each as a `stub.go` that the
owner deletes at integration because the owner's real file declares
the same names:

| Stub | Owner | Holds |
|---|---|---|
| `internal/agenttools/stub.go` | W3 | §4.1 verbatim, with `NewRegistry` and `ValidName` implemented |
| `plugins/agenttools/stub.go` | W3 | the complete `agent-tools` row |
| `plugins/engine/stub.go` | W2 | `engine-unreal`, whose Apply fails with `engine-unreal: not built yet (see go/docs/unreal-engine.md)` |

Git merges a stub beside its owner's real file without a conflict, and
the build then fails on the redeclared names, so the merge that brings
the owner's package in (W6's, last in the order) deletes the stub in
the same commit. W3's branch already carries `internal/agenttools/agenttools.go`
and `plugins/agenttools/agenttools.go`, and its `agent-tools` row and
comment in `go/bough.yml` match W6's byte for byte, so that hunk
merges clean.

No stubs were written for the other §3 packages: nothing on W6's branch
imports them, and each would be one more duplicate declaration to
delete. The e2e engine suite and the vtreal engine case skip on the
stub's error text, found by one `bough rows` probe, so they run
unchanged once W2's row lands.

### 18.2 The table

| WS | Scope | Owns (exclusive; nobody else edits these) | Provides (frozen in §4–§5) | Consumes | Done when |
|---|---|---|---|---|---|
| **W1** dependency + adapters | pin upkeep; Anthropic Messages adapter (streaming, tool_use, thinking, cache_control, retries, late-result render); Responses adapters for openai/openrouter/ollama with the SSE tap; envelope/late/strip/observe wrappers; provider selection and effort mapping in the llm rows; model catalogue bridge; fake (Load, FromStore) + echo + llm-script + llm-ollama; 1h cache pricing; retry.go `Type()` fix; probes P1–P7 | `go/go.mod`, `go/go.sum` (after B0); `go/internal/agentllm/**`; `go/internal/messagesapi/**`; `go/internal/unreal/{responses,wrap,fake,echo}/**`; `go/plugins/llm/**`; `go/internal/models/**`; `go/plugins/cost/**` | `agentllm.Source` on llm-anthropic / llm-openai / llm-openrouter / llm-ollama / llm-echo / llm-script; `messagesapi.New/Render/Decode/Spec`; `responses.New`; `wrap.*`; `fake.*`; `echo.New`; `llm.Usage.CacheWrite1hTokens`; `llm.Efforts` + `max` | `prompt.Wrap` (W5), for the property test only; a local copy until W5 merges | §15.1 W1 green, including the rapid property test; probe results (or a "skipped: no key" line each) in the messagesapi package doc; gates green on touched packages |
| **W2** engine row + session | coordinator per session; inbox wiring for input, steer, notices and ask answers; Gate (cancel, park, errors, budgets); ops.Manager with Latest replay; mirror + actor + the done/settle/wake rules; the loop's keys + `engine` + `drain`; store choice and paths; resume, seed, hseq catch-up, fork, subagents (Children); usage → `done.usage`; `/context`; stored notices; `bough engine inspect\|reproject\|script`; contract tests | `go/internal/unreal/{ops,session,contract}/**`; `go/plugins/engine/**`; `go/cmd/bough/enginecmd.go`; in `go/plugins/loop/loop.go` only `DefaultProject` (additive `call` folding, §12.2) | the `engine-unreal` row with prompt-sections, runner, inputs, cancel, steer, engine, drain; `session.Runtime`; `session.Children`; `ops.Manager` | everything in §5 via B0 stubs; `tool.NewRegistry` + harness bash for interim tests; real W1/W3/W4/W5 at integration | §15.1 W2 scenarios, §9.11 property test and §15.2 contract tests green; the echo smoke prints `ran: hi from codemode` on `engine-unreal` |
| **W3** tools + orbs | native registration of every bough tool; `toolreg` (translators, tombstones, view_image, Render); `boughcall` (hooks at the op boundary, timeout, spill, redaction); `Jobs.Adopt`; `run_js`; orb exec through the existing seam; secrets contract; portal/artifacts/lsp/todo/example; ask/secret as blocking calls; spawn via the `engine` key's `Spawn` + background agents | `go/internal/agenttools/**` (after B0); `go/plugins/agenttools/**`; `go/internal/unreal/{toolreg,boughcall}/**`; `go/plugins/{tools,ask,workers,artifacts,lsp,todo,example,codemode}/**`; `go/plugins/orb/portal.go`; `go/docs/PLUGINS.md` | `toolreg.New/Render/Hash`, `toolreg.Handle`, `boughcall.New`; native tools in `agent-tools`; `Jobs.Adopt`; `run_js` | `engine` key (W2) for foreground spawn; `agenttools.Hooks` impl (W5) | §15.1 W3 green; the loop's existing tests in the touched plugins stay green unchanged (codemode bindings byte-identical) |
| **W4** history + web + TUI | the projector (the §10.1 mapping table); every reader change in §10.4; call rows, call-delta, delta-reset, wake line, adopted rows in TUI and web; headless wake-done accounting, call-delta forwarding, drain at EOF; title/activity/cmux on `call`; bun tests, Playwright spec, `dist/app.js` rebuild | `go/internal/unreal/project/**`; `go/plugins/ui/**`; `go/internal/serve/**` (Go, `web/src`, `web/dist`, CSS via `dist/index.html` then `bun run design:sync`); `go/plugins/{title,activity,cmux}/**`; `go/tests/web/**` (new spec only) | `project.New/Meta/Item/Delta/Progress/Call`, `project.Out` | `toolreg.Render` + `Handle` (W3); `drain` key (W2); `llm-script` (W1) for Playwright | §15.1 W4 green; `bun test` + `bun run typecheck` green; goldens regenerated with `-update`; loop-session rendering unchanged (existing goldens untouched) |
| **W5** hooks + MCP + skills + context-md + rules | frozen prompt composition and drift reminders; `engine.md`; the contextbuilder decorator; hookbridge for all six events; skills catalogue + mention inject on the engine; AGENTS.md/MEMORY.md via context-md parts; rules gating inside bash/write/patch (tests); opt-in native MCP tools | `go/internal/unreal/{prompt,hookbridge}/**`; `go/plugins/{hooks,skills,contextmd,rules,mcp}/**`; `go/skills/multi-model-plan/SKILL.md`; hook docs in `go/docs/*.md` other than PLUGINS.md and this file | `prompt.Parts/Compose/Hash/Reminder/Wrap/Placeholder`; `hookbridge.New` (implements `agenttools.Hooks` + `session.Lifecycle`); `mcp__*` tools | agent-tools registry (B0); `session.Lifecycle` shape (§5.7) | §15.1 W5 green; a reminder round trip visible in a W2 session test once merged |
| **W6** tests + integration + removal plan | B0; `main.go` and `bough.yml` wiring; hermetic e2e on `llm-script` (text, tool call, fan-out, in-progress→final, ask, cancel, steer, resume, fork, background agent); one vtreal PTY scenario; pin + import-boundary tests; `scripts/unreal-smoke.sh`; harbor engine variant for the bench; additive docs; keeps `go build ./... && go vet ./...` green at every merge; §17 stays a plan (no deletions this run) | `go/cmd/bough/main.go`; `go/bough.yml`; `go/e2e/**` (new files); one new file in `go/internal/vtreal/`; `go/internal/unreal/{doc.go,pin_test.go,boundary_test.go}`; `scripts/unreal-smoke.sh`; `bench/harbor/bough_go_agent.py`; `README.md`, `AGENTS.md`, `go/README.md` (additive engine sections; the thesis unchanged); `go/docs/unreal-engine.md` | B0 stubs; the integration branch; the smoke script | all | full gates green after the last merge (`go vet ./...`, `go test -race -parallel 4 ./...`, bun); e2e green on the engine; smoke skips cleanly without keys; `plugin: loop` sessions unchanged |

### 18.3 Rules between workstreams

- **A file outside your "Owns" column is read-only for you.** If you
  need a change there, put the exact hunk in your final report under
  "requests for <WS>". W6 applies cross-owner hunks during integration.
  Nobody edits `plugins/loop` except W2's `DefaultProject` hunk.
- **Frozen signatures** (§4, §5, §5.8, §14.1) change only additively,
  and only with a line in the commit body naming the change. A breaking
  change needs the orchestrator.
- **Tests that need another workstream's real implementation** use the
  interim path the table names (harness `tool.NewRegistry`, a local
  Wrap copy, B0's fake) and switch to the real one at integration.
- **Merge order onto the integration branch:** B0 → W1 → W3 → W5 → W4
  → W2 → W6. Each merge rebases, then runs the full gate. A
  known-flaky test (`TestChildBurstStartsOnlyCap`, the vtreal PTY
  suite) is rerun alone before a merge is called red.
- **House rules:**
  - never `git stash`;
  - never `rm -rf` in a loop;
  - never restart or stop a running `bough serve`;
  - never push to origin;
  - never print a key;
  - commit subjects `scope: what changed`;
  - comments say why;
  - errors name the row and wrap.

## 19. Decisions for the user, and risks

### 19.1 Decisions this design deliberately leaves to the user

1. **Flip the default** (`go/bough.yml` `loop` row → `engine-unreal`).
   Do this after the §13 bench and a daily-driver soak with the
   overlay.
2. **Engine tool mode**, `native` or `both`. Keeping pure code mode as
   the product default honours the 2026-09-22 KEEP verdict until the
   bench says otherwise.
3. **Each removal R1–R8** (§17). They run against the standing rule
   "ask before replacing any existing surface".
4. **Store migration or lossy reseed** on a future harness format
   bump.
5. **`turn_settle`: 60s or 20s.** 60s keeps every call that fits
   today's 60s foreground bash limit in one turn; 20s mirrors
   ce6f5c23's `job_grace` for explicitly background commands.
6. **Default model.** The row stays `claude-sonnet-5`. Moving it to
   `claude-opus-5` turns server-side fallbacks on, and that is
   announced.

### 19.2 Risks

1. **Anthropic late-result rendering.** A render bug is a 400 on every
   turn that had a call slower than 1s. The Gate keeps Run alive, but
   the turn fails. The guards are the rapid property test (§15.1 W1)
   and probe P1.
2. **Preserved-thinking check** (Opus 5.5 and Fable 5.1, accounts
   created on or after 2026-08-31). Any mid-session edit is a 400. The
   system prompt is frozen, drift arrives as appended
   `<context-update>`, and display is fixed per session. The residual
   case is a tool-set change, which restarts the coordinator at idle
   with a new `tools` block. The guards:
   - `portal`, `write` and `patch` are registered from session start;
   - native MCP is opt-in;
   - on a thinking-binding 400 the adapter strips thinking and retries
     once.
3. **Done accounting is derived.** It mirrors coordinator state from
   items plus a settle timer. Drift causes a missing or duplicate
   `done`, which breaks hlPending, StatusOf and the web footer. The
   guards are one actor, two replicas of the same pure mirror, the
   §9.11 property test, and contract tests that pin the harness rules
   the mirror encodes.
4. **Park/mute correctness.** A bug either calls the model after Esc
   or swallows a real wake. Parking is keyed on call and input IDs,
   and the sync mirror is applied in the Observer before the next
   Respond.
5. **Turn semantics change.** A foreground call running past 60s of
   idle waiting splits into a job and a wake turn. A dev server
   started without `background: true` runs as an adopted job until the
   10m `call_timeout` kills it; engine.md teaches `background: true`.
   Every completion outside the 1s grace is a new full-context request,
   so long async sessions cost more than the loop. The 1h cache is
   what keeps them affordable.
6. **Unbounded context.** There is no trim and no compaction
   (auto-compaction is a hard no). Overflow is a sticky, recorded error
   that needs `/new` or a fork. Haiku 4.5 (200K) hits it first.
7. **Cost reporting.** It is wrong until three things land: 1h writes
   priced at 2× from `Usage.Raw`, the `claude-opus-5-5` override, and
   awareness that fallback turns bill at the fallback model's rates
   (`usage.iterations`).
8. **The harness is one day old** (v0.1.x, store format v2, v1 and
   "soft" sessions already unresumable). The mitigations are the SHA
   pin, the contract tests, the compat corpus, bough history as the
   canonical record, and the reseed fallback.
9. **OpenRouter serving `anthropic/*`** is unverified in two places:
   duplicate `function_call_output` (P5) and the signature round trip
   (P6). `LateResultsAsText` and `StripReasoningOn400` sidestep both,
   at the cost of images in late results and possibly reasoning
   continuity.
10. **The Responses streaming tap is best effort.** If it breaks, GPT
    turns go quiet until they complete (P7); results are unaffected.
11. **view_image restore coupling.** view_image is the one harness
    translator bough depends on for restore. A change to its output
    format on a bump rewrites replayed context, which the compat corpus
    catches.
12. **localfile cost.** It re-reads the whole JSONL on every
    Items/Resume page, and stores op state (base64 images) 2–3 times.
    Resume slows and disk grows with session length.
13. **Late results arrive as user-role text**, which is a larger
    prompt-injection surface than a tool_result block. engine.md names
    the format, and the bench should check whether Claude re-runs
    calls it thinks are pending.
14. **Secrets.** Anything a tool returns reaches the store and the
    provider, so redaction runs inside boughcall before
    `UpdateRemoteJob`, and `secret` never returns the value. A W3 test
    asserts no snapshot contains the secret.
15. **Process restart** (serve Stop) fails every running call as
    interrupted. That matches today, where Stop kills the child.
16. **The test cliff is avoided by deleting nothing.** The 38k lines of
    vtreal and 2.1k lines of e2e keep running on the loop, and the
    engine gets its own new layer.
