package main

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
)

// `bough browser ...` drives a real browser on the host. It exists for
// project sessions: the agent's shell runs in a container, and putting
// Chrome in every orb image would mean an ARM64 Chromium, its sandbox
// flags and its /dev/shm size in each one. The host already reaches
// every guest port (see internal/orb/portal.go), so the browser stays
// here and looks at the orb from outside.
//
// It is a pass-through to agent-browser, which keeps a background daemon
// and so holds element refs across separate invocations. That is the
// whole reason for going through it rather than an MCP server: bough
// runs an MCP server once per call, which throws the snapshot away
// between `snapshot` and `click`.
//
// Local sessions can run agent-browser directly; this is the relayed
// path (internal/orb/relay.go allow-lists "browser").

const browserBin = "agent-browser"

func runBrowser(args []string) {
	if len(args) == 0 {
		fmt.Fprintln(os.Stderr, "usage: bough browser <command> [args]   (agent-browser: open, snapshot -i, click @ref, ...)")
		os.Exit(2)
	}
	bin, err := exec.LookPath(browserBin)
	if err != nil {
		fmt.Fprintf(os.Stderr, "bough browser: %s is not installed on the host: install it with `npm i -g %s` or `brew install %s`, then run `%s install` once to fetch Chrome\n",
			browserBin, browserBin, browserBin, browserBin)
		os.Exit(127)
	}
	cmd := exec.Command(bin, args...)
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	// AGENT_BROWSER_SESSION is already set by the relay for a project
	// session, so each orb drives its own browser.
	cmd.Env = os.Environ()
	if err := cmd.Run(); err != nil {
		if ee, ok := errors.AsType[*exec.ExitError](err); ok {
			os.Exit(ee.ExitCode())
		}
		fmt.Fprintf(os.Stderr, "bough browser: %v\n", err)
		os.Exit(1)
	}
}
