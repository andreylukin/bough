package pipeline

import (
	"bytes"
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
)

// checkTail is how much of a failing check's output becomes {{input}}.
const checkTail = 200

// check runs the node's command with sh -c on the host. Checks run with
// the user's full privileges: the pipeline author wrote them, not an agent.
func (r *Runner) check(ctx context.Context, n *Node, sdir string, res *result) (routed string) {
	cctx := ctx
	if n.Timeout > 0 {
		var cancel context.CancelFunc
		cctx, cancel = context.WithTimeout(ctx, n.Timeout)
		defer cancel()
	}
	dir, err := r.checkDir(n)
	if err != nil {
		code := -1
		res.Exit = &code
		_ = os.WriteFile(filepath.Join(sdir, "output.txt"), []byte(err.Error()+"\n"), 0o644)
		r.event(Event{Kind: "check", Node: n.Name, Step: res.step, Text: n.Run, Data: map[string]any{"exit": code, "error": err.Error()}})
		res.Route, res.Reason = n.Fail, err.Error()
		return err.Error()
	}
	c := exec.CommandContext(cctx, "sh", "-c", n.Run)
	c.Dir = dir
	c.Env = append(os.Environ(), "BOUGH_LOOP_RUN="+r.id)
	c.WaitDelay = 2 * time.Second
	var out bytes.Buffer
	c.Stdout, c.Stderr = &out, &out
	err = c.Run()
	code, startErr := 0, ""
	if ee := new(exec.ExitError); errors.As(err, &ee) {
		code = ee.ExitCode()
	} else if err != nil {
		// The command never started (bad cwd, no sh): say why.
		code, startErr = -1, err.Error()
		out.WriteString(startErr + "\n")
	}
	_ = os.WriteFile(filepath.Join(sdir, "output.txt"), out.Bytes(), 0o644)
	res.Exit = &code
	data := map[string]any{"exit": code}
	if startErr != "" {
		data["error"] = startErr
	}
	r.event(Event{Kind: "check", Node: n.Name, Step: res.step, Text: n.Run, Data: data})
	switch {
	case ctx.Err() != nil:
		res.Reason = "stopped"
	case cctx.Err() == context.DeadlineExceeded:
		res.Route, res.Reason = n.Fail, "timeout"
	case startErr != "":
		res.Route, res.Reason = n.Fail, "exit -1: "+startErr
	case err != nil:
		res.Route, res.Reason = n.Fail, "exit "+itoa(code)
	default:
		res.Route, res.Reason = n.Pass, "exit 0"
	}
	return tail(out.String(), checkTail)
}

// checkDir resolves cwd: an agent node's worktree, else a path relative
// to the pipeline, else the pipeline's dir.
func (r *Runner) checkDir(n *Node) (string, error) {
	if n.Cwd == "" {
		return r.p.Dir, nil
	}
	if a, ok := r.p.Nodes[n.Cwd]; ok && a.Type == "agent" {
		if a.Mode != "project" {
			return r.p.Dir, nil
		}
		r.mu.Lock()
		sess := r.st.Sessions[a.Name]
		r.mu.Unlock()
		if sess == "" {
			return "", errors.New("cwd node " + a.Name + " has no session")
		}
		root := filepath.Join(r.opt.Home, ".bough", "orbs", sess)
		if proj, err := projectdef.Load(r.opt.Home, a.Project); err == nil && len(proj.Def.Repos) > 0 {
			return filepath.Join(root, proj.Def.Repos[0].RepoName()), nil
		}
		return root, nil
	}
	if filepath.IsAbs(n.Cwd) {
		return n.Cwd, nil
	}
	return filepath.Join(r.p.Dir, n.Cwd), nil
}

func tail(s string, lines int) string {
	parts := strings.SplitAfter(s, "\n")
	if len(parts) > 0 && parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	if len(parts) > lines {
		parts = parts[len(parts)-lines:]
	}
	return strings.Join(parts, "")
}
