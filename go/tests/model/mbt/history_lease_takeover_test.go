//go:build !windows

package mbt

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/plugins/history"
)

// specs/history_lease_takeover.fizz against the real history lease: the
// two processes that can hold a session (a terminal `bough -r` and a
// serve child) are two helper processes of this test binary, each
// running history.TakeLease / AppendFile on one shared session file, so
// the flock is contended across real pids exactly as in production. No
// serve is started: the lease is the file system's, and serve only reads
// it through history.LeaseHolder, which the adapter calls too.

const hltEnv = "HLT_HELPER"

// The helper is a line protocol on stdin/stdout: take, append, release,
// each answered with one line. It never reaches TestMain.
func init() {
	path := os.Getenv(hltEnv)
	if path == "" {
		return
	}
	var release func()
	in := bufio.NewScanner(os.Stdin)
	for in.Scan() {
		switch in.Text() {
		case "take":
			r, err := history.TakeLease(path)
			if err != nil {
				fmt.Println("refused")
				continue
			}
			release = r
			fmt.Println("ok")
		case "append":
			_, err := history.AppendFile(path, "note", map[string]any{"text": "hlt"})
			if err != nil {
				fmt.Println("err " + err.Error())
				continue
			}
			fmt.Println("ok")
		case "release":
			if release != nil {
				release()
				release = nil
			}
			fmt.Println("ok")
		}
	}
	os.Exit(0)
}

type hltProc struct {
	cmd *exec.Cmd
	in  interface{ Write([]byte) (int, error) }
	out *bufio.Scanner
}

func (p *hltProc) ask(msg string) (string, error) {
	if _, err := p.in.Write([]byte(msg + "\n")); err != nil {
		return "", err
	}
	if !p.out.Scan() {
		return "", fmt.Errorf("helper gone after %q", msg)
	}
	return p.out.Text(), nil
}

type historyLeaseAdapter struct {
	t    *testing.T
	gate gate

	path    string
	walk    int
	procs   map[string]*hltProc // "cli", "serve"
	refused int

	// dropRefusal is the deliberate bug TestHistoryLeaseTakeoverCatchesWrongAdapter
	// injects: a refused take is never counted.
	dropRefusal bool
}

func newHistoryLeaseAdapter(t *testing.T) *historyLeaseAdapter {
	a := &historyLeaseAdapter{t: t, procs: map[string]*hltProc{}}
	t.Cleanup(a.killAll)
	return a
}

func (a *historyLeaseAdapter) spawn(name string) error {
	cmd := exec.Command(os.Args[0])
	cmd.Env = append(os.Environ(), hltEnv+"="+a.path)
	in, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	out, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		return err
	}
	a.procs[name] = &hltProc{cmd: cmd, in: in, out: bufio.NewScanner(out)}
	return nil
}

func (a *historyLeaseAdapter) kill(name string) {
	if p := a.procs[name]; p != nil {
		p.cmd.Process.Kill()
		p.cmd.Wait()
		delete(a.procs, name)
	}
}

func (a *historyLeaseAdapter) killAll() {
	for n := range a.procs {
		a.kill(n)
	}
}

func (a *historyLeaseAdapter) Init() error {
	a.killAll()
	a.walk++
	dir := filepath.Join(a.t.TempDir(), fmt.Sprintf("w%d", a.walk))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	a.path = filepath.Join(dir, "s.jsonl")
	if err := os.WriteFile(a.path, nil, 0o644); err != nil {
		return err
	}
	a.refused = 0
	a.gate.reset()
	for _, n := range []string{"cli", "serve"} {
		if err := a.spawn(n); err != nil {
			return err
		}
	}
	return nil
}

func (a *historyLeaseAdapter) Cleanup() error { a.killAll(); return nil }

func (a *historyLeaseAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Lease", Index: 0}: a}, nil
}

func (a *historyLeaseAdapter) GetState() (map[string]any, error) {
	holder := "none"
	if pid := history.LeaseHolder(a.path); pid != 0 {
		for n, p := range a.procs {
			if p.cmd.Process.Pid == pid {
				holder = n
			}
		}
		if holder == "none" {
			return nil, fmt.Errorf("lease held by unknown pid %d", pid)
		}
	}
	raw, err := os.ReadFile(a.path)
	if err != nil {
		return nil, err
	}
	lines, corrupt := 0, false
	for _, l := range strings.Split(strings.TrimSuffix(string(raw), "\n"), "\n") {
		if l == "" {
			continue
		}
		var e history.Entry
		if json.Unmarshal([]byte(l), &e) != nil {
			corrupt = true
		}
		lines++
	}
	return map[string]any{
		"holder": holder, "cli_open": holder == "cli", "serve_open": holder == "serve",
		"lines": min(lines, 2), "corrupt": corrupt, "refused": a.refused,
	}, nil
}

func (a *historyLeaseAdapter) want(name, msg, ok string) error {
	got, err := a.procs[name].ask(msg)
	if err != nil {
		return err
	}
	if got != ok {
		return fmt.Errorf("%s %s: got %q, want %q", name, msg, got, ok)
	}
	return nil
}

func (a *historyLeaseAdapter) held() string {
	for n, p := range a.procs {
		if history.LeaseHolder(a.path) == p.cmd.Process.Pid {
			return n
		}
	}
	return "none"
}

func (a *historyLeaseAdapter) take(who string) error {
	if !a.gate.pass(a.held() == "none") {
		return nil
	}
	return a.want(who, "take", "ok")
}

func (a *historyLeaseAdapter) refuse(who, holder string) error {
	if !a.gate.pass(a.held() == holder) {
		return nil
	}
	if err := a.want(who, "take", "refused"); err != nil {
		return err
	}
	if !a.dropRefusal {
		a.refused = 1
	}
	return nil
}

func (a *historyLeaseAdapter) appendBy(who string) error {
	if !a.gate.pass(a.held() == who) {
		return nil
	}
	return a.want(who, "append", "ok")
}

func (a *historyLeaseAdapter) release(who string) error {
	if !a.gate.pass(a.held() == who) {
		return nil
	}
	return a.want(who, "release", "ok")
}

func (a *historyLeaseAdapter) CliTake() error          { return a.take("cli") }
func (a *historyLeaseAdapter) ServeTake() error        { return a.take("serve") }
func (a *historyLeaseAdapter) CliTakeRefused() error   { return a.refuse("cli", "serve") }
func (a *historyLeaseAdapter) ServeTakeRefused() error { return a.refuse("serve", "cli") }
func (a *historyLeaseAdapter) CliAppend() error        { return a.appendBy("cli") }
func (a *historyLeaseAdapter) ServeAppend() error      { return a.appendBy("serve") }
func (a *historyLeaseAdapter) CliRelease() error       { return a.release("cli") }
func (a *historyLeaseAdapter) ServeRelease() error     { return a.release("serve") }

// HolderCrash kills the holder outright; the kernel drops the flock.
// The dead process is replaced so the next take has a live taker.
func (a *historyLeaseAdapter) HolderCrash() error {
	h := a.held()
	if !a.gate.pass(h != "none") {
		return nil
	}
	a.kill(h)
	deadline := time.Now().Add(actionTimeout)
	for history.LeaseHolder(a.path) != 0 {
		if time.Now().After(deadline) {
			return fmt.Errorf("lease still held after the holder was killed")
		}
		time.Sleep(10 * time.Millisecond)
	}
	return a.spawn(h)
}

var historyLeaseActions = map[string]map[string]fmbt.ActionFunc{"Lease": {
	"CliTake":          action((*historyLeaseAdapter).CliTake),
	"ServeTake":        action((*historyLeaseAdapter).ServeTake),
	"CliTakeRefused":   action((*historyLeaseAdapter).CliTakeRefused),
	"ServeTakeRefused": action((*historyLeaseAdapter).ServeTakeRefused),
	"CliAppend":        action((*historyLeaseAdapter).CliAppend),
	"ServeAppend":      action((*historyLeaseAdapter).ServeAppend),
	"CliRelease":       action((*historyLeaseAdapter).CliRelease),
	"ServeRelease":     action((*historyLeaseAdapter).ServeRelease),
	"HolderCrash":      action((*historyLeaseAdapter).HolderCrash),
}}

func historyLeaseOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

func TestHistoryLeaseTakeover(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHistoryLeaseAdapter(t)
	if err := runMBT(t, "history_lease_takeover", a, historyLeaseActions, historyLeaseOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func TestHistoryLeaseTakeoverCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHistoryLeaseAdapter(t)
	a.dropRefusal = true
	if err := runMBT(t, "history_lease_takeover", a, historyLeaseActions, historyLeaseOptions()); err == nil {
		t.Fatal("a run that never counts a refused take passed; the runner is not checking state")
	}
}
