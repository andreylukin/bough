// Package orb is the "orb" plugin: a project session's container. It
// opens the session's orb (worktrees, image, running container), moves
// the process into the primary worktree, and provides the "orb" exec
// seam tools.bash runs through plus "orb-state" for the ui. A local
// session mounts nothing. See docs/orbs.md §2.
package orb

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
)

// chdir is swapped by tests: os.Chdir is process-global and would race
// every other parallel test.
var chdir = os.Chdir

// userHome is swapped by the mount test for the same reason as chdir:
// t.Setenv("HOME") cannot run in parallel.
var userHome = os.UserHomeDir

type pather interface{ Path() string }
type direr interface{ Dir() string }
type sections interface{ Set(name, text string) }

type plugin struct{}

// opened holds each session's orb across reloads of this row. The row
// reloads whenever a service it read lands late (the loop's
// prompt-sections always does), and stopping and reopening the
// container then would kill background jobs and rerun resume.sh.
var opened = struct {
	sync.Mutex
	m map[string]openOrb
}{m: map[string]openOrb{}}

type openOrb struct {
	o    *iorb.Orb
	slug string
	cfg  map[string]any
}

func init() {
	kernel.Register("orb", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string { return "orb" }

// Inject lists service keys: the session id comes from history and the
// scratch dir is mounted into the container at its host path.
func (plugin) Inject() []string { return []string{"history", "scratch"} }

// runtimeFor maps the row's runtime config to a backend.
func runtimeFor(name string) (container.Runtime, error) {
	switch name {
	case "":
		return container.Default(), nil
	case "apple":
		return container.NewApple(), nil
	case "nerdctl":
		return container.Nerdctl{}, nil
	case "podman":
		return container.Podman{}, nil
	case "fake":
		return container.NewFake(), nil
	}
	return nil, fmt.Errorf("orb: runtime %q: want apple, nerdctl, podman or fake", name)
}

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		if k != "runtime" {
			return fmt.Errorf("orb: unknown config key %q", k)
		}
	}
	if mode, _ := kernel.Get[string](ctx, "session-mode"); mode != "project" {
		// The read-only note is set here, not by tools: this row provides
		// nothing in local mode, so its reload when the loop's sections
		// land is free, where a tools reload cascades into loop and ui.
		if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
			s.Set("mode", iorb.LocalPromptSection)
			ctx.Effect(func() { s.Set("mode", "") })
		}
		return nil
	}
	slug, _ := kernel.Get[string](ctx, "session-project")
	name, _ := cfg["runtime"].(string)
	rt, err := runtimeFor(name)
	if err != nil {
		return err
	}
	h, err := kernel.Get[pather](ctx, "history")
	if err != nil {
		return fmt.Errorf("orb: %w", err)
	}
	pad, err := kernel.Get[direr](ctx, "scratch")
	if err != nil {
		return fmt.Errorf("orb: %w", err)
	}
	session := strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
	home, err := userHome()
	if err != nil {
		return fmt.Errorf("orb: home dir: %w", err)
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		return fmt.Errorf("orb: open %s: %w", slug, err)
	}
	// Open can build an image; bound it so a wedged engine fails the
	// row instead of hanging the mount forever.
	octx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
	defer cancel()
	if err := rt.Available(octx); err != nil {
		return fmt.Errorf("orb: open %s: runtime %s: %w (run `bough update` or `container system start`)", slug, rt.Name(), err)
	}
	opened.Lock()
	prev, reused := opened.m[session]
	delete(opened.m, session)
	opened.Unlock()
	reused = reused && prev.slug == slug && reflect.DeepEqual(prev.cfg, cfg)
	if !reused && prev.o != nil {
		stopOrb(prev.o)
	}
	o := prev.o
	if !reused {
		if o, err = iorb.Open(octx, rt, home, session, p, pad.Dir()); err != nil {
			return fmt.Errorf("orb: open %s: %w", slug, err)
		}
	}
	st := o.State()
	// Checkpoints and relative paths use the process cwd: it must be the
	// primary worktree, which is also the container's workdir.
	if st.Primary != "" {
		if err := chdir(st.Primary); err != nil {
			// Open started the container and no Effect owns it yet.
			stopOrb(o)
			return fmt.Errorf("orb: chdir %s: %w", st.Primary, err)
		}
	}
	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		s.Set("orb", promptSection(o.Root(), st, p.Def.Checks))
		ctx.Effect(func() { s.Set("orb", "") })
	}
	ctx.Provide("orb", o)
	ctx.Provide("orb-state", o)
	// Stop, never Remove: a resumed session reuses its worktrees and
	// container. A reload (this row still mounted and still desired as
	// is) keeps the orb running for the next Apply instead.
	ctx.Effect(func() {
		if reloading(ctx, cfg) {
			opened.Lock()
			opened.m[session] = openOrb{o: o, slug: slug, cfg: cfg}
			opened.Unlock()
			return
		}
		stopOrb(o)
	})
	return nil
}

// reloading reports whether the orb row is being disposed only to be
// applied again: it is still mounted (Unmount clears that first) and a
// desired row still carries the same config (Reconcile swaps the
// desired set before it disposes a removed or changed row).
func reloading(ctx *kernel.Context, cfg map[string]any) bool {
	active := map[string]bool{}
	for _, r := range ctx.Rows() {
		if r.Plugin == "orb" && r.State == kernel.StateActive {
			active[r.ID] = true
		}
	}
	for _, r := range ctx.Desired() {
		if active[r.ID] && !r.Disabled && reflect.DeepEqual(r.Config, cfg) {
			return true
		}
	}
	return false
}

func stopOrb(o *iorb.Orb) {
	sctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	if err := o.Stop(sctx); err != nil {
		fmt.Fprintf(os.Stderr, "bough: orb: stop: %v\n", err)
	}
}

// promptSection tells the model where its shell runs and what to check.
func promptSection(root string, st iorb.State, checks projectdef.Checks) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Project session: %s. Your shell runs in a Linux container (%s); files under %s are shared with the host at the same paths.\n", st.Project, st.Container, root)
	names := make([]string, 0, len(st.Worktrees))
	for n := range st.Worktrees {
		names = append(names, n)
	}
	sort.Strings(names)
	for _, n := range names {
		fmt.Fprintf(&b, "- repo %s: %s\n", n, st.Worktrees[n])
	}
	if checks.Fast != "" {
		fmt.Fprintf(&b, "Fast check: %s\n", checks.Fast)
	}
	if checks.Full != "" {
		fmt.Fprintf(&b, "Full check: %s\n", checks.Full)
	}
	return strings.TrimRight(b.String(), "\n")
}
