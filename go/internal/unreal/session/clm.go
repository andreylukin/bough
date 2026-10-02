//go:build !windows

package session

import (
	"context"
	"crypto/sha256"
	"fmt"
	"os"
	"path/filepath"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/contextkit"
	"github.com/andreylukin/bough/internal/unreal/clm"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

// Context files live where the ordinary file tools already write. This does
// not add a write root or bypass hooks, orb confinement, or permissions.
func (g *Gate) context() (*clm.Context, error) {
	g.clmMu.Lock()
	defer g.clmMu.Unlock()
	if g.contextForkMissing {
		return nil, fmt.Errorf("engine-clm: this fork point has no saved editable-context revision; restore its checkpoint or choose another turn")
	}
	if g.editable != nil {
		return g.editable, nil
	}
	id := g.contextID
	if id == "" {
		id = g.r.sid
	}
	dir := ""
	if g.r.d.Scratch != nil {
		dir = g.r.d.Scratch()
	}
	if dir == "" {
		dir = g.r.d.Cwd
	}
	if dir == "" {
		return nil, fmt.Errorf("engine-clm: no writable scratch or working directory for context")
	}
	contextDir := filepath.Join(dir, ".bough-clm")
	if err := os.MkdirAll(contextDir, 0700); err != nil {
		return nil, err
	}
	if st, err := os.Lstat(contextDir); err != nil || !st.IsDir() || st.Mode()&os.ModeSymlink != 0 {
		return nil, fmt.Errorf("engine-clm: context directory must be a real directory, not a symlink")
	}
	path := filepath.Join(contextDir, id+".md")
	state := filepath.Join(g.r.d.Store, id+".clm-state.json")
	if _, err := os.Stat(state); os.IsNotExist(err) && g.contextFork == "" {
		for _, e := range g.r.d.History.Entries() {
			if e.Kind == "engine" && e.Data["engine"] == "clm" && e.Data["session"] == id {
				return nil, fmt.Errorf("engine-clm: established session is missing its private cursor; restore it or start a new session")
			}
		}
	}
	if g.contextFork != "" {
		if _, err := os.Stat(state); os.IsNotExist(err) {
			if err = clm.Restore(g.contextFork, path, state); err != nil {
				return nil, err
			}
		}
	}
	c, err := clm.Open(path, state, g.r.cfg.CLMMaxBytes)
	if err != nil {
		return nil, err
	}
	canonical, err := agenttools.CanonicalFile(c.Path())
	if err != nil {
		return nil, err
	}
	g.r.contextsMu.Lock()
	if g.r.contexts == nil {
		g.r.contexts = make(map[string]*clm.Context)
	}
	g.r.contexts[canonical] = c
	g.r.contextsMu.Unlock()
	g.contextPath = canonical
	g.editable = c
	return c, nil
}

func (g *Gate) prepareContext(req ullm.Request) (ullm.Request, string, error) {
	c, err := g.context()
	if err != nil {
		return req, "", err
	}
	return c.PrepareRevision(req)
}

func (g *Gate) appendContext(out []ullm.Item) error {
	c, err := g.context()
	if err != nil {
		return err
	}
	return c.Append(out)
}

func (g *Gate) snapshotContext(turn string) (string, error) {
	g.clmMu.Lock()
	c := g.editable
	g.clmMu.Unlock()
	if c == nil || turn == "" {
		return "", nil
	}
	dir := filepath.Join(g.r.d.Store, g.r.sid+".clm-revisions")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return "", err
	}
	f, err := os.CreateTemp(dir, fmt.Sprintf("%x-*.json", sha256.Sum256([]byte(turn))))
	if err != nil {
		return "", err
	}
	path := f.Name()
	if err = f.Close(); err != nil {
		return "", err
	}
	if err = c.Snapshot(path); err != nil {
		os.Remove(path)
		return "", err
	}
	return path, nil
}

func (g *Gate) contextRevision() (string, error) {
	c, err := g.context()
	if err != nil {
		return "", err
	}
	return c.Revision()
}

func (r *Runtime) mutateContextFile(ctx context.Context, canonical string, mutate func() error) error {
	r.contextsMu.RLock()
	c := r.contexts[canonical]
	r.contextsMu.RUnlock()
	if c != nil {
		managed, err := c.MutateFile(ctx, canonical, mutate)
		if managed || err != nil {
			return err
		}
		return fmt.Errorf("engine-clm: managed context path changed; inspect it before retrying")
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	return mutate()
}

// A completed child's immutable read snapshots can be large. Its files
// remain on disk, but no active projection needs the native-write bridge.
func (g *Gate) releaseContext() {
	g.clmMu.Lock()
	path, editable := g.contextPath, g.editable
	g.clmMu.Unlock()
	g.r.contextsMu.Lock()
	if g.r.contexts[path] == editable {
		delete(g.r.contexts, path)
	}
	g.r.contextsMu.Unlock()
}

// Authorization stays with the live file row. A disabled/remounted write
// tool cannot leave an old context capability with broader permissions.
type authorizedContext struct {
	contextkit.Capability
	runtime *Runtime
	path    string
}

func (c authorizedContext) Edit(ctx context.Context, req contextkit.EditRequest) (contextkit.EditResult, error) {
	ctx, err := c.mutationContext(ctx, req.DryRun)
	if err != nil {
		return contextkit.EditResult{}, err
	}
	return c.Capability.Edit(ctx, req)
}

func (c authorizedContext) Offload(ctx context.Context, req contextkit.OffloadRequest) (contextkit.OffloadResult, error) {
	ctx, err := c.mutationContext(ctx, req.DryRun)
	if err != nil {
		return contextkit.OffloadResult{}, err
	}
	return c.Capability.Offload(ctx, req)
}

func (c authorizedContext) Restore(ctx context.Context, req contextkit.RestoreRequest) (contextkit.EditResult, error) {
	ctx, err := c.mutationContext(ctx, req.DryRun)
	if err != nil {
		return contextkit.EditResult{}, err
	}
	return c.Capability.Restore(ctx, req)
}

func (c authorizedContext) mutationContext(ctx context.Context, dry bool) (context.Context, error) {
	if dry {
		return ctx, nil
	}
	if err := ctx.Err(); err != nil {
		return ctx, err
	}
	if c.runtime.d.Tools != nil {
		if write, ok := c.runtime.d.Tools.Lookup("write"); ok && write.WriteAllowed != nil {
			// Capture the exact authorization before any permission wait.
			ctx = c.runtime.toolPolicyContext(ctx, write)
			return ctx, write.WriteAllowed(ctx, c.path)
		}
	}
	return ctx, fmt.Errorf("engine-clm: context mutations require the session's native write tool and its path policy")
}

func (r *Runtime) toolPolicyContext(ctx context.Context, tool agenttools.Tool) context.Context {
	if r.d.Tools == nil {
		return ctx
	}
	changed := r.d.Tools.Changed()
	return contextkit.WithCommitCheck(ctx, func() error {
		if current, ok := r.d.Tools.Lookup(tool.Name); !ok || !tool.SameRegistration(current) {
			return fmt.Errorf("engine-clm: native tool policy changed while the mutation was queued; retry with the current policy")
		}
		select {
		case <-changed:
			return fmt.Errorf("engine-clm: native tool policy changed while the mutation was queued; retry with the current policy")
		default:
			return ctx.Err()
		}
	})
}
