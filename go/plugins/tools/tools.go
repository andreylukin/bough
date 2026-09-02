// Package tools is the "tools-basic" plugin: bash, readFile, writeFile
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
	"sync"
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

// Stats is the "turn-stats" service: side-effect tallies of the basic
// tools, reset by Take.
type Stats struct {
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
	reg.RegisterTool("bash", st.bash)
	reg.RegisterTool("readFile", readFile)
	reg.RegisterTool("writeFile", st.writeFile)
	ctx.Provide("turn-stats", st)
	return nil
}

func (s *Stats) bash(cmd string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), bashTimeout)
	defer cancel()
	out, err := exec.CommandContext(ctx, "sh", "-c", cmd).CombinedOutput()
	if ctx.Err() == context.DeadlineExceeded {
		s.exited(-1)
		return "", fmt.Errorf("bash: killed after %s: %s\n%s", bashTimeout, cmd, out)
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

func readFile(path string) (string, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

func (s *Stats) writeFile(path, content string) (string, error) {
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		return "", err
	}
	s.wrote(path)
	return fmt.Sprintf("wrote %d bytes to %s", len(content), path), nil
}
