// Package tools is the "tools-basic" plugin: bash, readFile, writeFile
// registered into the codemode service.
package tools

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"time"

	"github.com/andreylukin/bough/kernel"
)

const bashTimeout = 60 * time.Second

// registry is the slice of the codemode service we need.
type registry interface {
	RegisterTool(name string, fn any)
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
	reg.RegisterTool("bash", bash)
	reg.RegisterTool("readFile", readFile)
	reg.RegisterTool("writeFile", writeFile)
	return nil
}

func bash(cmd string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), bashTimeout)
	defer cancel()
	out, err := exec.CommandContext(ctx, "sh", "-c", cmd).CombinedOutput()
	if ctx.Err() == context.DeadlineExceeded {
		return "", fmt.Errorf("bash: timeout after %s\n%s", bashTimeout, out)
	}
	if err != nil {
		return "", fmt.Errorf("bash: %v\n%s", err, out)
	}
	return string(out), nil
}

func readFile(path string) (string, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

func writeFile(path, content string) (string, error) {
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		return "", err
	}
	return fmt.Sprintf("wrote %d bytes to %s", len(content), path), nil
}
