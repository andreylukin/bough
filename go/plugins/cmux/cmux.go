// Package cmux names the cmux workspace after the session. cmux (a
// macOS terminal for agents) puts each tab in a sidebar; a bough
// running inside one renames its tab to the session's title as soon
// as the title plugin picks one, and to the title on file when a
// session is resumed. Outside cmux the row mounts and does nothing.
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
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// candidates is where the cmux CLI lives when it is not on PATH: the
// app bundles it.
var candidates = []string{"/Applications/cmux.app/Contents/Resources/bin/cmux"}

// Namer renames one workspace.
type Namer struct {
	cli       string
	workspace string
	run       func(ctx context.Context, args ...string) error
	prefix    string

	mu    sync.Mutex
	named string // the last title set
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

// Name renames the workspace to title (with the prefix), once per
// distinct title.
func (n *Namer) Name(title string) {
	title = strings.TrimSpace(title)
	if title == "" {
		return
	}
	n.mu.Lock()
	if n.named == title {
		n.mu.Unlock()
		return
	}
	n.named = title
	n.mu.Unlock()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = n.run(ctx, "rename-workspace", "--workspace", n.workspace, "--", n.prefix+title)
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

// History is the seam for the title on file.
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
	n := &Namer{cli: cli, workspace: workspace, run: cliRunner(cli), prefix: prefix}
	kctx.Provide("cmux", n)
	// A resumed session already has its name on file.
	if h, err := kernel.Get[History](kctx, "history"); err == nil {
		for _, e := range h.Entries() {
			if e.Kind == "title" {
				if t, _ := e.Data["text"].(string); t != "" {
					go n.Name(t)
				}
			}
		}
	}
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok && ev.Kind == "title" {
			go n.Name(ev.Text)
		}
	})
	return nil
}
