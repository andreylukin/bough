package orb

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/ci/ciflags"
)

// The guest has no usable bough: the host binary is a macOS build, and
// MCP servers need the host's keychain grants and local programs. So
// `bough mcp ...` in the orb is a shim that asks the host proxy to run
// the host's own bough and hands back its output.

// relayedCommands are the bough subcommands the guest may run on the host.
// project definitions live in the host's ~/.bough/projects, and the
// browser is the host's: a Chrome in the guest would mean an ARM64
// Chromium per image, and the host can already reach the guest's ports.
//
// ci is relayed read-only (see ciRelayCheck): the results cache is in the
// host's ~/.bough, keyed by the worktree's tree, so the guest can ask what
// is settled for the files it sees. Running a check is not relayed: the
// commands come from .bough/ci.yml, a file the agent in the orb can
// write, and running them on the host would put agent-written shell
// outside the container that confines the session's file changes — and
// on the host's toolchain rather than the project image's. So an orb
// session's checks run only when a person runs `bough ci` on the host
// in its worktree; running them in the container, against a CI worktree
// the orb mounts, is the missing half (docs/orbs.md).
var relayedCommands = map[string]bool{"mcp": true, "project": true, "browser": true, "ci": true}

const relayTimeout = 10 * time.Minute

// The guest speaks a base64 framing rather than JSON: a project may
// bring its own Dockerfile, and then the only interpreter it is fair to
// assume is bash. Base64 keeps arbitrary argv and stdin (newlines,
// quotes, binary) out of the framing without an escaping pass the shim
// would have to implement by hand.
//
// request:  one base64 arg per line, a blank line, then base64 stdin
// response: exit code, base64 stdout, base64 stderr, one per line

// parseRelayBody reads the framed request.
func parseRelayBody(b []byte) (args []string, stdin string, err error) {
	head, rest, _ := strings.Cut(string(b), "\n\n")
	for _, line := range strings.Split(head, "\n") {
		if line = strings.TrimSpace(line); line == "" {
			continue
		}
		a, err := base64.StdEncoding.DecodeString(line)
		if err != nil {
			return nil, "", fmt.Errorf("arg: %w", err)
		}
		args = append(args, string(a))
	}
	if rest = strings.TrimSpace(rest); rest != "" {
		in, err := base64.StdEncoding.DecodeString(rest)
		if err != nil {
			return nil, "", fmt.Errorf("stdin: %w", err)
		}
		stdin = string(in)
	}
	return args, stdin, nil
}

// writeRelayBody writes the framed response.
func writeRelayBody(w io.Writer, stdout, stderr string, exit int) {
	fmt.Fprintf(w, "%d\n%s\n%s\n", exit,
		base64.StdEncoding.EncodeToString([]byte(stdout)),
		base64.StdEncoding.EncodeToString([]byte(stderr)))
}

func (p *proxy) relayExec(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 8<<20))
	if err != nil {
		http.Error(w, "orb relay: "+err.Error(), http.StatusBadRequest)
		return
	}
	args, stdin, err := parseRelayBody(body)
	if err != nil {
		http.Error(w, "orb relay: "+err.Error(), http.StatusBadRequest)
		return
	}
	if len(args) == 0 || !relayedCommands[args[0]] {
		http.Error(w, "orb relay: only `bough mcp ...`, `bough project ...`, `bough browser ...` and `bough ci --no-wait|log` run on the host", http.StatusForbidden)
		return
	}
	dir := ""
	if home, err := os.UserHomeDir(); err == nil {
		dir = home
	}
	if args[0] == "ci" {
		d, err := p.ciRelayCheck(args[1:], r.Header.Get(relayCwdHeader))
		if err != nil {
			http.Error(w, "orb relay: "+err.Error(), http.StatusForbidden)
			return
		}
		dir = d
	}
	bin, err := p.bin()
	if err != nil {
		http.Error(w, "orb relay: "+err.Error(), http.StatusInternalServerError)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), relayTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.Dir = dir
	// Each orb drives its own browser. Two sessions sharing one would
	// race over the active tab, focus, dialogs and the ref map, so the
	// session id becomes the browser session name. BOUGH_SESSION and
	// BOUGH_RELAYED tell `bough project restart` whose orb asked, so the
	// agent needs no session id and the request is recorded as its own.
	// It grants nothing new: every `bough project` verb is relayed already.
	if s := r.Header.Get(relaySessionHeader); s != "" {
		cmd.Env = append(os.Environ(), "AGENT_BROWSER_SESSION="+s, "BOUGH_SESSION="+s, "BOUGH_RELAYED=1")
	}
	var stdout, stderr bytes.Buffer
	cmd.Stdin, cmd.Stdout, cmd.Stderr = bytes.NewBufferString(stdin), &stdout, &stderr
	exit := 0
	if err := cmd.Run(); err != nil {
		exit = -1
		if ee, ok := errors.AsType[*exec.ExitError](err); ok {
			exit = ee.ExitCode()
		} else {
			stderr.WriteString("orb relay: " + err.Error() + "\n")
		}
	}
	// The shim reads the body as three lines. Go chunks a response it
	// cannot size (anything past 2KB written before the handler returns),
	// and the shim then read a chunk-size line as the exit code, the
	// exit code as stdout ("base64: invalid input") and stdout as
	// stderr, with exit 1: every MCP result over 2KB failed in an orb.
	// Sizing the body keeps it one unframed stream.
	var framed bytes.Buffer
	writeRelayBody(&framed, stdout.String(), stderr.String(), exit)
	w.Header().Set("Content-Type", "text/plain")
	w.Header().Set("Content-Length", strconv.Itoa(framed.Len()))
	_, _ = w.Write(framed.Bytes())
}

// relayCwdHeader carries the guest shell's working directory. The
// worktrees are mounted at their host paths, so it names the same
// checkout on the host; only `bough ci` uses it.
const relayCwdHeader = "X-Bough-Cwd"

// ciRelayCheck admits a guest's `bough ci` only when it cannot run a
// check (--no-wait, or `ci log`), only from inside the orb's dir, and
// without --dir (which would point it elsewhere on the host). It returns
// the host directory to run in.
//
// The args are parsed with the host's own flag set, so what is checked
// is what the host binary will do: flag lets the last --no-wait win, and
// a scan that only ever turned read-only on let `--no-wait
// --no-wait=false` run agent-written checks on the host.
func (p *proxy) ciRelayCheck(args []string, cwd string) (string, error) {
	var fs *flag.FlagSet
	if len(args) > 0 && args[0] == "log" {
		fs, _ = ciflags.NewLogFlagSet("")
		fs.SetOutput(io.Discard)
		if _, err := ciflags.ParseInterleaved(fs, args[1:]); err != nil {
			return "", fmt.Errorf("`bough ci log`: %v", err)
		}
	} else {
		var fl *ciflags.RunFlags
		fs, fl = ciflags.NewRunFlagSet("")
		fs.SetOutput(io.Discard)
		if err := fs.Parse(args); err != nil {
			return "", fmt.Errorf("`bough ci`: %v", err)
		}
		if fs.NArg() > 0 {
			return "", fmt.Errorf("`bough ci`: unexpected argument %q", fs.Arg(0))
		}
		if !*fl.NoWait {
			return "", errors.New("`bough ci` cannot run checks from an orb: they run on the host only, and only when a person runs `bough ci` there for this worktree. " +
				"`bough ci --no-wait` reports the results stored for this tree, `bough ci log <check>` a stored log")
		}
	}
	if ciflags.FlagSet(fs, "dir") {
		return "", errors.New("`bough ci --dir` is not relayed: run it from the worktree")
	}
	if p.root == "" || cwd == "" || !filepath.IsAbs(cwd) {
		return "", fmt.Errorf("ci cwd %q is outside the orb", cwd)
	}
	root, err := filepath.EvalSymlinks(p.root)
	if err != nil {
		return "", fmt.Errorf("ci: orb dir: %w", err)
	}
	real, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return "", fmt.Errorf("ci cwd %q is outside the orb", cwd)
	}
	if rel, err := filepath.Rel(root, real); err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("ci cwd %q is outside the orb", cwd)
	}
	return real, nil
}

// relaySessionHeader carries the guest's session id, so a relayed
// command can scope per-orb state to it.
const relaySessionHeader = "X-Bough-Session"

// shimScript is the guest's `bough`: it posts its args (and stdin when
// piped) to $BOUGH_HOST and replays the host's output and exit code.
//
// bash, not python3: the base image has python3, but a project may bring
// its own Dockerfile and then the shim died with "python3: not found"
// and nothing said why. bash speaks HTTP well enough through /dev/tcp,
// needs no package, and its redirection bypasses the proxy env the orb
// sets (the relay is the proxy's own listener, reached directly).
const shimScript = `#!/bin/bash
# bough in a project orb: runs ` + "`bough mcp|project|browser|ci ...`" + ` on the host
# (see internal/orb/relay.go). Needs only bash and base64.
set -u
if [ -z "${BOUGH_HOST:-}" ]; then
  echo "bough: no host relay in this orb (BOUGH_HOST unset)" >&2
  exit 1
fi
hostport=${BOUGH_HOST#*://}
hostport=${hostport%%/*}
host=${hostport%:*}
port=${hostport##*:}

b64() { base64 | tr -d '\n'; }

body=""
for a in "$@"; do
  body="$body$(printf '%s' "$a" | b64)
"
done
body="$body
"
if [ ! -t 0 ]; then
  body="$body$(b64)"
fi

# CR must be a real carriage return, so it is built at runtime: this
# script lives in a Go raw string, where \r would stay two characters.
CR=$'\r'
req="POST /bough/exec HTTP/1.1$CR
Host: $hostport$CR
Content-Type: text/plain$CR
Content-Length: ${#body}$CR
Connection: close$CR
"
if [ -n "${BOUGH_ORB_TOKEN:-}" ]; then
  req="${req}Authorization: Bearer $BOUGH_ORB_TOKEN$CR
"
fi
if [ -n "${BOUGH_SESSION:-}" ]; then
  req="${req}X-Bough-Session: $BOUGH_SESSION$CR
"
fi
case $PWD in
  *$'\n'*|*$'\r'*) ;;
  *) req="${req}X-Bough-Cwd: $PWD$CR
" ;;
esac
req="$req$CR
$body"

if ! exec 3<>"/dev/tcp/$host/$port"; then
  echo "bough: cannot reach the host relay at $host:$port" >&2
  exit 1
fi
printf '%s' "$req" >&3

IFS= read -r status <&3 || { echo "bough: no answer from the host relay" >&2; exit 1; }
while IFS= read -r line <&3; do
  [ "${line%$'\r'}" = "" ] && break
done
case $status in
  *" 200 "*) ;;
  *) sed -e 's/^/bough: /' <&3 >&2; exit 1 ;;
esac

IFS= read -r code <&3
IFS= read -r out <&3
IFS= read -r err <&3
[ -n "${out:-}" ] && printf '%s' "$out" | base64 -d
[ -n "${err:-}" ] && printf '%s' "$err" | base64 -d >&2
case ${code:-1} in
  ''|*[!0-9-]*) exit 1 ;;
  -*) exit 1 ;;
esac
exit "$code"
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
