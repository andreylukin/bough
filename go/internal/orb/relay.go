package orb

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// The guest has no usable bough: the host binary is a macOS build, and
// MCP servers need the host's keychain grants and local programs. So
// `bough mcp ...` in the orb is a shim that asks the host proxy to run
// the host's own bough and hands back its output.

// relayedCommands are the bough subcommands the guest may run on the host.
var relayedCommands = map[string]bool{"mcp": true}

// hostBough is the binary relayed calls run; tests swap it.
var hostBough = func() (string, error) { return os.Executable() }

const relayTimeout = 10 * time.Minute

type relayRequest struct {
	Args  []string `json:"args"`
	Stdin string   `json:"stdin,omitempty"`
}

type relayResponse struct {
	Stdout string `json:"stdout"`
	Stderr string `json:"stderr"`
	Exit   int    `json:"exit"`
}

func relayExec(w http.ResponseWriter, r *http.Request) {
	var req relayRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 8<<20)).Decode(&req); err != nil {
		http.Error(w, "orb relay: "+err.Error(), http.StatusBadRequest)
		return
	}
	if len(req.Args) == 0 || !relayedCommands[req.Args[0]] {
		http.Error(w, "orb relay: only `bough mcp ...` runs on the host", http.StatusForbidden)
		return
	}
	bin, err := hostBough()
	if err != nil {
		http.Error(w, "orb relay: "+err.Error(), http.StatusInternalServerError)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), relayTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, req.Args...)
	if home, err := os.UserHomeDir(); err == nil {
		cmd.Dir = home
	}
	var stdout, stderr bytes.Buffer
	cmd.Stdin, cmd.Stdout, cmd.Stderr = bytes.NewBufferString(req.Stdin), &stdout, &stderr
	resp := relayResponse{}
	if err := cmd.Run(); err != nil {
		resp.Exit = -1
		if ee, ok := errors.AsType[*exec.ExitError](err); ok {
			resp.Exit = ee.ExitCode()
		} else {
			stderr.WriteString("orb relay: " + err.Error() + "\n")
		}
	}
	resp.Stdout, resp.Stderr = stdout.String(), stderr.String()
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp)
}

// shimScript is the guest's `bough`: it posts its args (and stdin when
// piped) to $BOUGH_HOST and replays the host's output and exit code.
const shimScript = `#!/bin/sh
# bough in a project orb: runs ` + "`bough mcp ...`" + ` on the host (see internal/orb/relay.go).
exec python3 -c '
import json, os, sys, urllib.request
host = os.environ.get("BOUGH_HOST")
if not host:
    sys.exit("bough: no host relay in this orb (BOUGH_HOST unset)")
stdin = "" if sys.stdin.isatty() else sys.stdin.read()
req = urllib.request.Request(host + "/bough/exec", data=json.dumps({"args": sys.argv[1:], "stdin": stdin}).encode(), headers={"Content-Type": "application/json"})
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
try:
    resp = json.load(opener.open(req, timeout=660))
except urllib.error.HTTPError as e:
    sys.exit("bough: " + e.read().decode().strip())
sys.stdout.write(resp["stdout"]); sys.stderr.write(resp["stderr"])
sys.exit(resp["exit"] if resp["exit"] >= 0 else 1)
' "$@"
`

// shimDir is where the shim lives: under the scratchpad, which every orb
// already mounts at its host path, so running orbs get it without a
// container restart.
func shimDir(scratch string) string { return filepath.Join(scratch, ".bin") }

func writeShim(scratch string) error {
	dir := shimDir(scratch)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(dir, "bough"), []byte(shimScript), 0o755)
}
