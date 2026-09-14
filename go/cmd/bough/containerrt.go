package main

import (
	"context"
	"fmt"
	"io"
	"os/exec"
	"strings"
	"time"
)

// homebrewContainer is where brew puts Apple's container CLI on Apple
// silicon; a login shell may lack /opt/homebrew/bin on PATH.
const homebrewContainer = "/opt/homebrew/bin/container"

// ensureContainerRuntime installs and starts Apple's container CLI on
// darwin. It warns and returns; it never fails the update: project
// sessions need it, local sessions (the default) do not.
func ensureContainerRuntime(out io.Writer, run func(name string, args ...string) error, look func(string) (string, error), goos string) {
	if goos != "darwin" {
		return
	}
	warn := func(what, fix string) {
		fmt.Fprintf(out, "bough: container runtime: %s (%s)\n", what, fix)
	}
	bin, err := look("container")
	if err != nil {
		// A path with a slash makes LookPath check that file directly,
		// which keeps the fallback behind the injected seam.
		bin, err = look(homebrewContainer)
	}
	if err != nil {
		if _, berr := look("brew"); berr != nil {
			warn("Apple container CLI missing and Homebrew not found", "install Homebrew then `brew install container`")
			return
		}
		fmt.Fprintln(out, "bough: installing Apple container CLI (brew install container)…")
		if err := run("brew", "install", "container"); err != nil {
			warn("brew install container failed: "+err.Error(), "run `brew install container`")
			return
		}
		bin = homebrewContainer
		if p, lerr := look("container"); lerr == nil {
			bin = p
		}
	}
	if run(bin, "system", "status") == nil {
		return
	}
	fmt.Fprintln(out, "bough: starting container system…")
	// --enable-kernel-install is unverified; plain start is the fallback.
	if run(bin, "system", "start", "--enable-kernel-install") == nil {
		return
	}
	if err := run(bin, "system", "start"); err != nil {
		warn("container system start failed: "+err.Error(), "run `container system start`")
	}
}

// runQuiet runs a command with stdin from /dev/null (so a kernel-download
// prompt cannot hang the update) under a 5-minute bound, keeping output
// only to explain a failure.
func runQuiet(name string, args ...string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	b, err := exec.CommandContext(ctx, name, args...).CombinedOutput()
	if err != nil {
		msg := strings.TrimSpace(string(b))
		if len(msg) > 300 {
			msg = "…" + msg[len(msg)-300:]
		}
		if msg != "" {
			return fmt.Errorf("%s %s: %w: %s", name, strings.Join(args, " "), err, msg)
		}
		return fmt.Errorf("%s %s: %w", name, strings.Join(args, " "), err)
	}
	return nil
}
