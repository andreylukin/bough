//go:build !windows

package llm

// llm-control: a deterministic model a test steers turn by turn. Each
// engine request takes the lexically first <name>.json in the control
// dir (renaming it <name>.taken, so the test can see the call is in
// flight) and answers as it says: finish with text, fail, stream slowly,
// call one tool, or hold until <name>.release appears. llm-script's tape is fixed at
// mount; this one is fed while the session runs, which is what cancel,
// steer and "the model is still thinking" tests need. The test side is
// go/tests/model/llm. Config: dir (default ~/.bough/llm-control), and
// hold_boot (hold a fresh session before its history file; see holdBoot).

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"golang.org/x/sys/unix"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-control", func() kernel.Plugin { return &controlPlugin{} })
}

type controlPlugin struct{}

func (p *controlPlugin) Name() string     { return "llm-control" }
func (p *controlPlugin) Inject() []string { return nil }

func (p *controlPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	dir, _ := cfg["dir"].(string)
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("llm-control: no dir configured and no home: %w", err)
		}
		dir = filepath.Join(home, ".bough", "llm-control")
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("llm-control: %w", err)
	}
	holdStart(dir)
	if hold := cfg["hold_boot"]; hold == true || hold == "true" {
		if err := holdBoot(ctx, dir); err != nil {
			return err
		}
	}
	ctx.Provide(serviceKey(cfg), &controlLLM{dir: dir, tag: newControlTag()})
	return nil
}

// holdStart parks the process while <dir>/start.hold exists, announcing
// itself as start-<pid>.held, and exits 3 if start.exit appears. The llm
// row mounts before history, so a held session has no history file yet:
// that is how a test puts serve's Create between spawn and discovery,
// or makes the child die there.
func holdStart(dir string) {
	hold := filepath.Join(dir, "start.hold")
	if _, err := os.Stat(hold); err != nil {
		return
	}
	held := filepath.Join(dir, fmt.Sprintf("start-%d.held", os.Getpid()))
	os.WriteFile(held, nil, 0o644)
	defer os.Remove(held)
	for {
		if _, err := os.Stat(filepath.Join(dir, "start.exit")); err == nil {
			os.Remove(held)
			os.Exit(3)
		}
		if _, err := os.Stat(hold); err != nil {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// holdBoot keeps a fresh session from going on to write its history
// file until the test writes <dir>/boot/<id>.release. serve's Create
// waits for that file, so this is how a model test holds a project's
// main thread (or a thread) in "starting" for as long as the step it is
// checking needs; the orb's own start is async and never held it. This
// row mounts before history, so the wait is in front of the file. A
// session whose file exists is a restart or a reload and goes on.
func holdBoot(ctx *kernel.Context, dir string) error {
	id, _ := kernel.Get[string](ctx, "session-id")
	if id == "" {
		return nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("llm-control: hold_boot: %w", err)
	}
	if _, err := os.Stat(filepath.Join(home, ".bough", "history", id+".jsonl")); err == nil {
		return nil
	}
	boot := filepath.Join(dir, "boot")
	if err := os.MkdirAll(boot, 0o755); err != nil {
		return fmt.Errorf("llm-control: hold_boot: %w", err)
	}
	role := "session"
	if isMain, _ := kernel.Get[bool](ctx, "session-main"); isMain {
		role = "main"
	}
	// The pid first: a test that finds the session waiting can then
	// tell whether this process outlives what serve did with it.
	if err := os.WriteFile(filepath.Join(boot, id+".pid"), []byte(strconv.Itoa(os.Getpid())), 0o644); err != nil {
		return fmt.Errorf("llm-control: hold_boot: %w", err)
	}
	if err := os.WriteFile(filepath.Join(boot, id+".waiting"), []byte(role), 0o644); err != nil {
		return fmt.Errorf("llm-control: hold_boot: %w", err)
	}
	// Bounded so a test that never releases fails on its own timeout
	// rather than leaving a process behind.
	deadline := time.Now().Add(2 * time.Minute)
	for time.Now().Before(deadline) {
		// <id>.exit: the child dies before its history file, the way a
		// crash in its first second would.
		if _, err := os.Stat(filepath.Join(boot, id+".exit")); err == nil {
			os.Exit(3)
		}
		if _, err := os.Stat(filepath.Join(boot, id+".release")); err == nil {
			// <id>.nostdin: serve's write of the first prompt fails
			// while the child lives on with its file.
			if _, err := os.Stat(filepath.Join(boot, id+".nostdin")); err == nil {
				if err := detachStdin(); err != nil {
					return fmt.Errorf("llm-control: hold_boot: nostdin: %w", err)
				}
			}
			return nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return fmt.Errorf("llm-control: hold_boot: %s never released", id)
}

// detachStdin swaps fd 0 for a pipe this process keeps both ends of:
// serve's end of the old stdin loses its only reader, so its next write
// fails (EPIPE), and the child's own reads block instead of seeing an
// EOF that would shut it down (and delete its file).
func detachStdin() error {
	var p [2]int
	if err := unix.Pipe(p[:]); err != nil {
		return err
	}
	if err := unix.Dup2(p[0], 0); err != nil {
		return err
	}
	return unix.Close(p[0])
}

type controlLLM struct {
	dir string
	// tag makes this process's response ids its own. A provider's ids
	// are unique; numbering from 1 in every process was not, and a
	// respawned child's first call reused the id of the call its
	// predecessor made, which the engine's projector already had as
	// done: the new call ran with no call events at all.
	tag string

	mu    sync.Mutex // serialises taking a turn across session views
	n     int
	usage Usage
}

var _ agentllm.Source = (*controlLLM)(nil)

// Complete does not take a turn, for the reason llm-script's does not:
// title and status jobs fall back to the main llm, and one that took a
// queued turn would leave the session's request answering the wrong one.
func (c *controlLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	return "control", nil
}

func (c *controlLLM) Model() string { return "control" }
func (c *controlLLM) Ready() error  { return nil }

func (c *controlLLM) Usage() Usage {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.usage
}

func (c *controlLLM) AgentAdapter(o agentllm.Options) (agentllm.Adapter, error) {
	return &controlAdapter{c: c, opts: o}, nil
}

type controlTurn struct {
	Mode    string       `json:"mode"`
	Text    string       `json:"text"`
	Error   string       `json:"error"`
	DelayMS int          `json:"delay_ms"`
	Call    *controlCall `json:"call"`
	// Tool and Args are a "call" turn's one tool call.
	Tool string          `json:"tool"`
	Args json.RawMessage `json:"args"`
	// Bash, on a release, answers with one bash tool call running it.
	Bash string `json:"bash"`
	// Calls, on an "ok" turn, are tool calls the response makes instead
	// of text, so a test can have the agent run a real tool (a shell edit
	// the write tools never report).
	Calls []controlCall `json:"calls"`
}

// controlCall is a tool call a release answers with, so a test can put
// a turn through a real native call and keep it running after.
type controlCall struct {
	ID   string          `json:"id"`
	Name string          `json:"name"`
	Args json.RawMessage `json:"args"`
}

// take claims the next queued turn. A name the test is still writing
// ends in .tmp and is skipped, so a half-written file is never read.
func (c *controlLLM) take() (name string, t controlTurn, ok bool, err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	ents, err := os.ReadDir(c.dir)
	if err != nil {
		return "", t, false, err
	}
	names := []string{}
	for _, e := range ents {
		if n, ok := strings.CutSuffix(e.Name(), ".json"); ok && !e.IsDir() {
			names = append(names, n)
		}
	}
	if len(names) == 0 {
		return "", t, false, nil
	}
	slices.Sort(names)
	name = names[0]
	src := filepath.Join(c.dir, name+".json")
	b, err := os.ReadFile(src)
	if err != nil {
		return "", t, false, err
	}
	if err := json.Unmarshal(b, &t); err != nil {
		return "", t, false, fmt.Errorf("llm-control: %s.json: %w", name, err)
	}
	if err := os.Rename(src, filepath.Join(c.dir, name+".taken")); err != nil {
		return "", t, false, err
	}
	c.n++
	return name, t, true, nil
}

type controlAdapter struct {
	c    *controlLLM
	opts agentllm.Options
}

func (a *controlAdapter) Provider() string { return "control" }
func (a *controlAdapter) Model() string    { return "control" }
func (a *controlAdapter) Close() error     { return nil }

func (a *controlAdapter) Respond(ctx context.Context, r ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	if err := ctx.Err(); err != nil {
		return ullm.Response{}, err
	}
	name, turn, ok, err := a.c.take()
	if err != nil {
		return ullm.Response{}, err
	}
	// An empty queue answers instead of waiting, so a test that queued
	// too few turns fails on its output rather than timing out.
	if !ok {
		return a.reply(ctx, "[llm-control: no turn queued in "+a.c.dir+"]", 0)
	}
	switch turn.Mode {
	case "ok", "":
		if len(turn.Calls) > 0 {
			return a.calls(ctx, name, turn.Calls)
		}
		return a.reply(ctx, turn.Text, 0)
	case "error":
		return ullm.Response{}, errors.New(turn.Error)
	case "slow":
		return a.reply(ctx, turn.Text, time.Duration(turn.DelayMS)*time.Millisecond)
	case "call":
		return a.callTurn(ctx, name, turn)
	case "block":
		release := filepath.Join(a.c.dir, name+".release")
		stream := filepath.Join(a.c.dir, name+".stream")
		seq := agentllm.SeqOf(ctx)
		tick := time.NewTicker(10 * time.Millisecond)
		defer tick.Stop()
		for {
			// A fragment streamed while held: text on screen that no
			// entry records yet. Removing the file is the test's signal
			// that it went out.
			if b, err := os.ReadFile(stream); err == nil {
				if a.opts.Sink != nil {
					a.opts.Sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaText, Text: string(b)})
				}
				if err := os.Remove(stream); err != nil {
					return ullm.Response{}, err
				}
			}
			if b, err := os.ReadFile(release); err == nil {
				// A release that carries a turn answers as it says, so a
				// test can hold a session in "running" and only then
				// choose whether the turn finishes or fails.
				var then controlTurn
				if len(b) > 0 && json.Unmarshal(b, &then) == nil && then.Mode == "error" {
					return ullm.Response{}, errors.New(then.Error)
				}
				if then.Call != nil {
					return a.call(ctx, then.Text, *then.Call)
				}
				if then.Mode == "call" {
					return a.callTurn(ctx, name, then)
				}
				// A release with a command answers with a bash call: the
				// engine records it with its exit and asks again, so a
				// test can put a tool call inside a turn it still holds.
				if then.Bash != "" {
					return a.bash(ctx, name, then.Bash)
				}
				if then.Text != "" {
					return a.reply(ctx, then.Text, 0)
				}
				return a.reply(ctx, turn.Text, 0)
			}
			if err := a.say(ctx, name); err != nil {
				return ullm.Response{}, err
			}
			select {
			case <-ctx.Done():
				return ullm.Response{}, ctx.Err()
			case <-tick.C:
			}
		}
	}
	return ullm.Response{}, fmt.Errorf("llm-control: %s.json: unknown mode %q (want ok, error, slow, call or block)", name, turn.Mode)
}

// say streams each <name>.say-<n> a held turn has been handed as one
// live delta and renames it <name>.said-<n>, in the order of n. It is
// text the session shows and never records, so a test can make the
// ephemeral path fire while the turn is still in flight.
func (a *controlAdapter) say(ctx context.Context, name string) error {
	says, err := filepath.Glob(filepath.Join(a.c.dir, name+".say-*"))
	if err != nil {
		return err
	}
	slices.Sort(says)
	for _, p := range says {
		if strings.HasSuffix(p, "-tmp") {
			continue
		}
		b, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		if a.opts.Sink != nil {
			a.opts.Sink(agentllm.Delta{Seq: agentllm.SeqOf(ctx), Attempt: 1, Kind: agentllm.DeltaText, Text: string(b)})
		}
		n := strings.TrimPrefix(filepath.Base(p), name+".say-")
		if err := os.Rename(p, filepath.Join(a.c.dir, name+".said-"+n)); err != nil {
			return err
		}
	}
	return nil
}

// reply streams text a word at a time, delay apart, so the live
// assistant-delta path runs as it does against a real provider.
func (a *controlAdapter) reply(ctx context.Context, text string, delay time.Duration) (ullm.Response, error) {
	seq := agentllm.SeqOf(ctx)
	for i, w := range controlWords(text) {
		if i > 0 && delay > 0 {
			select {
			case <-ctx.Done():
				return ullm.Response{}, ctx.Err()
			case <-time.After(delay):
			}
		}
		if a.opts.Sink != nil {
			a.opts.Sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaText, Text: w})
		}
	}
	u := ullm.Usage{InputTokens: 1, OutputTokens: 1}
	a.c.mu.Lock()
	addAgentUsage(&a.c.usage, u)
	n := a.c.n
	a.c.mu.Unlock()
	return ullm.Response{
		ID:     fmt.Sprintf("control-%s-%d", a.c.tag, n),
		Stop:   ullm.StopComplete,
		Output: []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: text}}},
		Usage:  u,
	}, nil
}

// call answers with text (when any) and then a tool call, the shape of
// a provider response that goes on to run a tool.
func (a *controlAdapter) call(ctx context.Context, text string, c controlCall) (ullm.Response, error) {
	r, err := a.reply(ctx, text, 0)
	if err != nil {
		return r, err
	}
	if text == "" {
		r.Output = nil
	}
	if c.ID == "" {
		c.ID = r.ID + "-call"
	}
	args := string(c.Args)
	if args == "" || args == "null" {
		args = "{}"
	}
	r.Output = append(r.Output, ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: c.ID, Name: c.Name, Arguments: args}})
	return r, nil
}

// callTurn answers a "call" turn: one call of its Tool, so a test can
// make the model ask (tools.ask, tools.secret) at a moment it picks.
// The call's result goes out on the next request, which takes the next
// queued turn.
func (a *controlAdapter) callTurn(ctx context.Context, name string, turn controlTurn) (ullm.Response, error) {
	if turn.Tool == "" {
		return ullm.Response{}, fmt.Errorf("llm-control: %s: a call turn needs a tool", name)
	}
	return a.call(ctx, "", controlCall{Name: turn.Tool, Args: turn.Args})
}

// bash answers with one bash tool call running cmd.
func (a *controlAdapter) bash(ctx context.Context, name, cmd string) (ullm.Response, error) {
	args, err := json.Marshal(map[string]string{"command": cmd})
	if err != nil {
		return ullm.Response{}, err
	}
	call := ullm.ToolCall{CallID: "control_" + name, Name: "bash", Arguments: string(args)}
	if a.opts.Sink != nil {
		a.opts.Sink(agentllm.Delta{Seq: agentllm.SeqOf(ctx), Attempt: 1, Kind: agentllm.DeltaToolStart, CallID: call.CallID, Name: call.Name})
	}
	u := ullm.Usage{InputTokens: 1, OutputTokens: 1}
	a.c.mu.Lock()
	addAgentUsage(&a.c.usage, u)
	n := a.c.n
	a.c.mu.Unlock()
	return ullm.Response{
		ID:     fmt.Sprintf("control-%s-%d", a.c.tag, n),
		Stop:   ullm.StopComplete,
		Output: []ullm.Item{{Type: ullm.ItemToolCall, Data: call}},
		Usage:  u,
	}, nil
}

// calls answers with tool calls only; the engine runs them and makes the
// next request, which takes the next queued turn.
func (a *controlAdapter) calls(ctx context.Context, name string, cs []controlCall) (ullm.Response, error) {
	seq := agentllm.SeqOf(ctx)
	var out []ullm.Item
	for i, c := range cs {
		id := c.ID
		if id == "" {
			id = fmt.Sprintf("control_%s_%d", name, i+1)
		}
		args := string(c.Args)
		if args == "" || args == "null" {
			args = "{}"
		}
		if a.opts.Sink != nil {
			a.opts.Sink(agentllm.Delta{Seq: seq, Attempt: 1, Kind: agentllm.DeltaToolStart, CallID: id, Name: c.Name})
		}
		out = append(out, ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: id, Name: c.Name, Arguments: args}})
	}
	u := ullm.Usage{InputTokens: 1, OutputTokens: 1}
	a.c.mu.Lock()
	addAgentUsage(&a.c.usage, u)
	n := a.c.n
	a.c.mu.Unlock()
	return ullm.Response{ID: fmt.Sprintf("control-%s-%d", a.c.tag, n), Stop: ullm.StopComplete, Output: out, Usage: u}, nil
}

func controlWords(s string) []string {
	var out []string
	for s != "" {
		i := strings.IndexAny(s, " \n")
		if i < 0 {
			return append(out, s)
		}
		out = append(out, s[:i+1])
		s = s[i+1:]
	}
	return out
}

// newControlTag is 8 random hex digits, fresh per process.
func newControlTag() string {
	var b [4]byte
	if _, err := rand.Read(b[:]); err != nil {
		return strconv.Itoa(os.Getpid())
	}
	return hex.EncodeToString(b[:])
}
