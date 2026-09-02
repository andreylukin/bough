// Package tools is the "tools-basic" plugin: bash, view, patch
// registered into the codemode service. It also provides "turn-stats":
// the files written and the last bash exit code since the last Take,
// which the loop stamps onto its end-of-turn "done" entry.
package tools

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// bashTimeout is the tools.bash kill deadline (documented in the loop's
// system prompt; a var so tests can shorten it).
var bashTimeout = 60 * time.Second

// registry is the slice of the codemode service we need.
type registry interface {
	RegisterTool(name string, fn any)
}

// runContexter is the optional slice of codemode that exposes the
// running script's context: the turn's cancel reaches tools.bash
// through it.
type runContexter interface {
	RunContext() context.Context
}

// Stats is the "turn-stats" service: side-effect tallies of the basic
// tools, reset by Take.
type Stats struct {
	runCtx func() context.Context // the running script's context; nil = none

	mu    sync.Mutex
	files []string
	exit  int
	ran   bool // a bash call happened since the last Take
}

// Take returns the files written and the last bash exit code (ran is
// false when no bash call happened) since the previous Take, and resets.
func (s *Stats) Take() (files []string, exit int, ran bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	files, exit, ran = s.files, s.exit, s.ran
	s.files, s.exit, s.ran = nil, 0, false
	return files, exit, ran
}

func (s *Stats) wrote(path string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.files = append(s.files, path)
}

func (s *Stats) exited(code int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.exit, s.ran = code, true
}

type plugin struct{}

func init() {
	kernel.Register("tools-basic", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "tools-basic" }
func (plugin) Inject() []string { return []string{"codemode"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	reg, err := kernel.Get[registry](ctx, "codemode")
	if err != nil {
		return err
	}
	st := &Stats{}
	if rc, ok := reg.(runContexter); ok {
		st.runCtx = rc.RunContext
	}
	reg.RegisterTool("bash", st.bash)
	reg.RegisterTool("view", view)
	reg.RegisterTool("patch", st.patch)
	ctx.Provide("turn-stats", st)
	return nil
}

func (s *Stats) bash(cmd string) (string, error) {
	parent := context.Background()
	if s.runCtx != nil {
		parent = s.runCtx()
	}
	ctx, cancel := context.WithTimeout(parent, bashTimeout)
	defer cancel()
	c := exec.CommandContext(ctx, "sh", "-c", cmd)
	// Its own process group, killed as a group: `sh -c` execs or forks
	// the command, and killing sh alone leaves a sleep, a server, a
	// build running after the turn was cancelled.
	c.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	c.Cancel = func() error { return syscall.Kill(-c.Process.Pid, syscall.SIGKILL) }
	c.WaitDelay = 2 * time.Second
	out, err := c.CombinedOutput()
	if ctx.Err() == context.DeadlineExceeded {
		s.exited(-1)
		return "", fmt.Errorf("bash: killed after %s: %s\n%s", bashTimeout, cmd, out)
	}
	if ctx.Err() == context.Canceled {
		s.exited(-1)
		return "", fmt.Errorf("bash: cancelled: %s\n%s", cmd, out)
	}
	if err != nil {
		code := -1
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			code = ee.ExitCode()
		}
		s.exited(code)
		return "", fmt.Errorf("bash: %v\n%s", err, out)
	}
	s.exited(0)
	return string(out), nil
}

// view returns a file's lines numbered "N│text", optionally only
// lines start..end (1-based, inclusive; end 0 = to the end). Numbers
// make patch targets and error lines easy to refer to.
func view(path string, rng ...int) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	lines := strings.Split(strings.TrimSuffix(string(data), "\n"), "\n")
	start, end := 1, len(lines)
	if len(rng) > 0 && rng[0] > 0 {
		start = rng[0]
	}
	if len(rng) > 1 && rng[1] > 0 && rng[1] < end {
		end = rng[1]
	}
	if start > len(lines) {
		return "", fmt.Errorf("view: %s has %d lines, start %d is past the end", path, len(lines), start)
	}
	width := len(strconv.Itoa(end))
	var b strings.Builder
	for n := start; n <= end; n++ {
		fmt.Fprintf(&b, "%*d│%s\n", width, n, lines[n-1])
	}
	return b.String(), nil
}

// patch replaces one exact occurrence of old with new in path. old
// must match exactly once (include more context when it repeats). An
// empty old creates the file with new when it does not exist yet.
func (s *Stats) patch(path, old, new string) (string, error) {
	data, err := os.ReadFile(path)
	if old == "" {
		if err == nil {
			return "", fmt.Errorf("patch: %s exists; give the text to replace (old) or create a new path", path)
		}
		if !errors.Is(err, os.ErrNotExist) {
			return "", err
		}
		if dir := filepath.Dir(path); dir != "." {
			if err := os.MkdirAll(dir, 0o755); err != nil {
				return "", err
			}
		}
		if err := os.WriteFile(path, []byte(new), 0o644); err != nil {
			return "", err
		}
		s.wrote(path)
		return fmt.Sprintf("created %s (%d bytes)", path, len(new)), nil
	}
	if err != nil {
		return "", err
	}
	switch n := strings.Count(string(data), old); n {
	case 0:
		return "", fmt.Errorf("patch: old text not found in %s (view it and copy the exact lines)", path)
	case 1:
	default:
		return "", fmt.Errorf("patch: old text occurs %d times in %s; include more surrounding lines", n, path)
	}
	out := strings.Replace(string(data), old, new, 1)
	if err := os.WriteFile(path, []byte(out), 0o644); err != nil {
		return "", err
	}
	s.wrote(path)
	return fmt.Sprintf("patched %s (%+d lines)", path,
		strings.Count(new, "\n")-strings.Count(old, "\n")), nil
}
