// Package cmux puts the session on its cmux sidebar row. cmux (a macOS
// terminal for agents) shows each workspace as a row with a name, a
// description line under it, and status pills; a bough running inside
// one fills all three:
//
//   - the name is the session's title, as soon as the title plugin
//     picks one, and the title on file when a session is resumed;
//   - the description is what the agent is working on — the prompt of
//     the turn in flight, and after it, the prompt it last answered;
//   - the "bough" pill is whether it is running: "working" (or the
//     activity plugin's label of what it is doing right now), "needs
//     an answer" while tools.ask waits on you, "done · waiting for you"
//     and "stopped" when a turn ends.
//
// The description and pill are cleared when bough exits, so the shell
// left behind does not look like a finished agent. Outside cmux the
// row mounts and does nothing.
package cmux

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/ask"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// candidates is where the cmux CLI lives when it is not on PATH: the
// app bundles it.
var candidates = []string{"/Applications/cmux.app/Contents/Resources/bin/cmux"}

// statusKey is the pill bough owns; other tools keep theirs.
const statusKey = "bough"

// Pill colours and SF Symbol icons per state.
var (
	working = pill{icon: "bolt.fill", color: "#ff9500"}
	asking  = pill{icon: "questionmark.circle.fill", color: "#5ac8fa"}
	done    = pill{icon: "checkmark.circle.fill", color: "#34c759"}
	stopped = pill{icon: "stop.circle.fill", color: "#8e8e93"}
)

type pill struct{ icon, color string }

// Namer drives one workspace's row.
type Namer struct {
	cli       string
	workspace string
	run       func(ctx context.Context, args ...string) error
	prefix    string

	mu      sync.Mutex
	named   string // the last title set
	desc    string // the last description set
	status  string // the last pill text set
	running bool   // a turn is in flight
	queue   chan func()
}

// Detect reports the cmux CLI and workspace id when this process runs
// inside a cmux terminal, else "".
func Detect(env func(string) string, look func(string) (string, error), stat func(string) (os.FileInfo, error)) (cli, workspace string) {
	workspace = env("CMUX_WORKSPACE_ID")
	if workspace == "" {
		return "", ""
	}
	if p, err := look("cmux"); err == nil {
		return p, workspace
	}
	for _, c := range candidates {
		if _, err := stat(c); err == nil {
			return c, workspace
		}
	}
	return "", ""
}

// newNamer wires a Namer whose CLI calls run one at a time, in order,
// off the event goroutine: "working" must land before "done".
func newNamer(cli, workspace, prefix string, run func(ctx context.Context, args ...string) error) *Namer {
	n := &Namer{cli: cli, workspace: workspace, run: run, prefix: prefix, queue: make(chan func(), 64)}
	go func() {
		for f := range n.queue {
			f()
		}
	}()
	return n
}

// call queues one cmux invocation; a full queue drops it (the next
// state change supersedes it anyway) rather than stalling the loop.
func (n *Namer) call(args ...string) {
	n.enqueue(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = n.run(ctx, args...)
	})
}

// enqueue hands f to the worker, dropping it when the queue is full.
func (n *Namer) enqueue(f func()) {
	select {
	case n.queue <- f:
	default:
	}
}

// Name renames the workspace to title (with the prefix), once per
// distinct title.
func (n *Namer) Name(title string) {
	title = strings.TrimSpace(title)
	if title == "" {
		return
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.named == title {
		return
	}
	n.named = title
	n.call("rename-workspace", "--workspace", n.workspace, "--", n.prefix+title)
}

// Describe sets the row's description line, once per distinct text.
func (n *Namer) Describe(text string) {
	text = strings.TrimSpace(strings.SplitN(text, "\n", 2)[0])
	if text == "" {
		return
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.desc == text {
		return
	}
	n.desc = text
	n.call("workspace-action", "--workspace", n.workspace, "--action", "set-description", "--description", text)
}

// Status sets the bough pill, once per distinct text.
func (n *Namer) Status(p pill, text string) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if n.status == text {
		return
	}
	n.status = text
	n.call("set-status", statusKey, text, "--workspace", n.workspace, "--icon", p.icon, "--color", p.color, "--priority", "80")
}

// Clear takes the pill and description off the row: bough is leaving.
func (n *Namer) Clear() {
	n.mu.Lock()
	n.call("clear-status", statusKey, "--workspace", n.workspace)
	if n.desc != "" {
		n.call("workspace-action", "--workspace", n.workspace, "--action", "clear-description")
	}
	n.mu.Unlock()
	// Drain: the effect runs at exit, and the calls must land first.
	doneCh := make(chan struct{})
	n.enqueue(func() { close(doneCh) })
	select {
	case <-doneCh:
	case <-time.After(3 * time.Second):
	}
}

// Event folds one loop event into the row. The loop announces no turn
// start, so the first content of a turn is what flips the pill to
// working — and the moment the description picks up the prompt, from
// history's last input entry.
func (n *Namer) Event(kind, text string, lastPrompt func() string) {
	switch kind {
	case "assistant-delta", "thinking-delta", "thinking", "assistant", "code", "result", "steer":
		n.mu.Lock()
		was := n.running
		n.running = true
		n.mu.Unlock()
		if !was {
			n.Status(working, "working")
			if lastPrompt != nil {
				n.Describe(lastPrompt())
			}
		}
	case "activity":
		n.mu.Lock()
		on := n.running
		n.mu.Unlock()
		if on && strings.TrimSpace(text) != "" {
			n.Status(working, text)
		}
	case "ask":
		// Answering it brings content, which flips the pill back.
		n.mu.Lock()
		n.running = false
		n.mu.Unlock()
		n.Status(asking, "needs an answer")
	case "done":
		n.mu.Lock()
		n.running = false
		n.mu.Unlock()
		n.Status(done, "done · waiting for you")
	case "cancelled":
		n.mu.Lock()
		n.running = false
		n.mu.Unlock()
		n.Status(stopped, "stopped")
	}
}

func cliRunner(cli string) func(ctx context.Context, args ...string) error {
	return func(ctx context.Context, args ...string) error {
		out, err := exec.CommandContext(ctx, cli, args...).CombinedOutput()
		if err != nil {
			return fmt.Errorf("cmux %s: %w: %s", args[0], err, strings.TrimSpace(string(out)))
		}
		return nil
	}
}

// ---------- plugin ----------

type plugin struct{}

func init() {
	kernel.Register("cmux", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "cmux" }
func (plugin) Inject() []string { return nil }

// History is the seam for the title and prompts on file.
type History interface{ Entries() []history.Entry }

func (plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	prefix := ""
	for k, v := range cfg {
		switch k {
		case "prefix":
			prefix, _ = v.(string)
		default:
			return fmt.Errorf("cmux: unknown config key %q", k)
		}
	}
	cli, workspace := Detect(os.Getenv, exec.LookPath, os.Stat)
	if cli == "" {
		return nil // not in cmux: nothing to name
	}
	n := newNamer(cli, workspace, prefix, cliRunner(cli))
	kctx.Provide("cmux", n)
	var lastPrompt func() string
	// A resumed session already has its name on file.
	if h, err := kernel.Get[History](kctx, "history"); err == nil {
		lastPrompt = func() string { return history.LastPrompt(h.Entries()) }
		n.Describe(lastPrompt()) // what it was about, before anything runs
		for _, e := range h.Entries() {
			if e.Kind == "title" {
				if t, _ := e.Data["text"].(string); t != "" {
					n.Name(t)
				}
			}
		}
	}
	kctx.On("loop/event", func(p any) {
		switch ev := p.(type) {
		case loop.Event:
			if ev.Kind == "title" {
				n.Name(ev.Text)
				return
			}
			n.Event(ev.Kind, ev.Text, lastPrompt)
		case ask.Event:
			n.Event(ev.Kind, ev.Text, lastPrompt)
		}
	})
	kctx.Effect(n.Clear)
	return nil
}
