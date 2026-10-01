//go:build !windows

package session

import (
	"crypto/sha256"
	"fmt"
	"os"
	"path/filepath"

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
	g.editable = c
	return c, nil
}

func (g *Gate) prepareContext(req ullm.Request) (ullm.Request, error) {
	g.r.contextFileMu.Lock()
	defer g.r.contextFileMu.Unlock()
	c, err := g.context()
	if err != nil {
		return req, err
	}
	return c.Prepare(req)
}

func (g *Gate) appendContext(out []ullm.Item) error {
	g.r.contextFileMu.Lock()
	defer g.r.contextFileMu.Unlock()
	c, err := g.context()
	if err != nil {
		return err
	}
	return c.Append(out)
}

func (g *Gate) snapshotContext(turn string) (string, error) {
	g.r.contextFileMu.Lock()
	defer g.r.contextFileMu.Unlock()
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
	g.r.contextFileMu.Lock()
	defer g.r.contextFileMu.Unlock()
	c, err := g.context()
	if err != nil {
		return "", err
	}
	return c.Revision()
}
