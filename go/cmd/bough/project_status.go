package main

import (
	"context"
	"fmt"
	"io"

	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

// projectStatus prints the preflight: what a session start needs, one
// line per check, and fails when any check does.
func projectStatus(out io.Writer, home, slug string) error {
	p, err := projectdef.Load(home, slug)
	if err != nil {
		return err
	}
	failed := 0
	for _, c := range orb.Preflight(context.Background(), home, projectRuntime(), p) {
		line := fmt.Sprintf("%-5s %-8s %s", c.Status, c.Kind, c.Name)
		if c.Detail != "" {
			line += "  " + c.Detail
		}
		fmt.Fprintln(out, line)
		if c.Status == orb.PreflightFail {
			failed++
		}
	}
	switch failed {
	case 0:
		return nil
	case 1:
		return fmt.Errorf("1 preflight check failed")
	}
	return fmt.Errorf("%d preflight checks failed", failed)
}
