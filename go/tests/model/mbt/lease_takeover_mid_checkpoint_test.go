//go:build !windows

package mbt

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/plugins/history"
)

// specs/lease_takeover_mid_checkpoint.fizz against the real checkpoint
// steps (plugins/history/tree.go Snapshot/PinRef, history.go AppendFile):
// two processes race a session's lease and its three-step checkpoint
// (Snapshot -> Append -> Pin) across crashes and restarts. Like
// history_lease_takeover_test.go, each "process" is a real helper
// subprocess of this test binary so the flock is contended across real
// pids; no serve is started, since the mechanism under test is the git +
// jsonl layer serve and the loop both call straight through.
//
// A helper's Append recomputes its own tree via Snapshot when it holds
// none: write-tree is content-addressed, so a process that never ran
// Snapshot itself still gets the identical hash for an unchanged working
// tree, exactly as if it had. That is what lets a second holder pick up
// a first holder's Append after a crash with no special-casing.

const ltmcEnv = "LTMC_HELPER"

// The helper is a line protocol on stdin/stdout: take, snapshot, append,
// pin, each answered with one line ("ok" or "err <msg>"). It never
// reaches TestMain.
func init() {
	dir := os.Getenv(ltmcEnv)
	if dir == "" {
		return
	}
	session := filepath.Join(dir, "session.jsonl")
	var (
		release     func()
		pendingTree string
		pendingSeq  int64
	)
	reply := func(err error) {
		if err != nil {
			fmt.Println("err " + err.Error())
			return
		}
		fmt.Println("ok")
	}
	in := bufio.NewScanner(os.Stdin)
	for in.Scan() {
		switch in.Text() {
		case "take":
			r, err := history.TakeLease(session)
			if err == nil {
				release = r
			}
			reply(err)
		case "snapshot":
			tree, err := history.Snapshot(dir)
			if err == nil {
				pendingTree = tree
			}
			reply(err)
		case "append":
			if pendingTree == "" {
				tree, err := history.Snapshot(dir)
				if err != nil {
					reply(err)
					continue
				}
				pendingTree = tree
			}
			e, err := history.AppendFile(session, "checkpoint", map[string]any{"checkpoint": pendingTree})
			if err == nil {
				pendingSeq = e.Seq
			}
			reply(err)
		case "pin":
			reply(history.PinRef(dir, "s", pendingSeq, pendingTree))
		case "forget":
			// NextTurn: this turn's pending tree/seq are done with,
			// whether pinned or left dangling.
			pendingTree, pendingSeq = "", 0
			fmt.Println("ok")
		}
		if release != nil {
			_ = release // kept alive; released only by process death (Crash)
		}
	}
	os.Exit(0)
}

type ltmcProc struct {
	cmd *exec.Cmd
	in  interface{ Write([]byte) (int, error) }
	out *bufio.Scanner
}

func (p *ltmcProc) ask(msg string) (string, error) {
	if _, err := p.in.Write([]byte(msg + "\n")); err != nil {
		return "", err
	}
	if !p.out.Scan() {
		return "", fmt.Errorf("helper gone after %q", msg)
	}
	return p.out.Text(), nil
}

// leaseTakeoverMidCheckpointAdapter is the Checkpoint role from
// specs/lease_takeover_mid_checkpoint.fizz. p1/p2's *_dead, seq, has_tree,
// entry_written, author, pinned, dangling_exists and pin_lost are the
// adapter's own bookkeeping (mirroring the spec's require exactly, so the
// gate and GetState never disagree); holder comes off the real flock via
// history.LeaseHolder, and every state-changing step is a real call
// through a helper subprocess.
type leaseTakeoverMidCheckpointAdapter struct {
	t    *testing.T
	gate gate

	dir     string
	session string
	walk    int
	procs   map[string]*ltmcProc // "p1", "p2"

	holder                        string // "p1", "p2", "none"
	p1Dead, p2Dead                bool
	seq                           int
	hasTree, entryWritten, pinned bool
	author                        string // "p1", "p2", "none"
	danglingExists, pinLost       bool

	// dropPinLost is the deliberate bug TestLeaseTakeoverMidCheckpointCatchesWrongAdapter
	// injects: pin_lost never gets set, so a stale Pin looks legal.
	dropPinLost bool
}

func newLeaseTakeoverMidCheckpointAdapter(t *testing.T) *leaseTakeoverMidCheckpointAdapter {
	a := &leaseTakeoverMidCheckpointAdapter{t: t, procs: map[string]*ltmcProc{}}
	t.Cleanup(a.killAll)
	return a
}

func (a *leaseTakeoverMidCheckpointAdapter) spawn(name string) error {
	cmd := exec.Command(os.Args[0])
	cmd.Env = append(os.Environ(), ltmcEnv+"="+a.dir)
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
	a.procs[name] = &ltmcProc{cmd: cmd, in: in, out: bufio.NewScanner(out)}
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) kill(name string) {
	if p := a.procs[name]; p != nil {
		p.cmd.Process.Kill()
		p.cmd.Wait()
		delete(a.procs, name)
	}
}

func (a *leaseTakeoverMidCheckpointAdapter) killAll() {
	for n := range a.procs {
		a.kill(n)
	}
}

func (a *leaseTakeoverMidCheckpointAdapter) Init() error {
	a.killAll()
	a.walk++
	a.dir = filepath.Join(a.t.TempDir(), fmt.Sprintf("w%d", a.walk))
	if err := os.MkdirAll(a.dir, 0o755); err != nil {
		return err
	}
	if err := exec.Command("git", "-C", a.dir, "init", "-q").Run(); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(a.dir, "f.txt"), []byte("seed"), 0o644); err != nil {
		return err
	}
	a.session = filepath.Join(a.dir, "session.jsonl")
	if err := os.WriteFile(a.session, nil, 0o644); err != nil {
		return err
	}
	a.holder, a.author = "none", "none"
	a.p1Dead, a.p2Dead = false, false
	a.seq = 0
	a.hasTree, a.entryWritten, a.pinned, a.danglingExists, a.pinLost = false, false, false, false, false
	a.gate.reset()
	for _, n := range []string{"p1", "p2"} {
		if err := a.spawn(n); err != nil {
			return err
		}
	}
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) Cleanup() error { a.killAll(); return nil }

func (a *leaseTakeoverMidCheckpointAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Checkpoint", Index: 0}: a}, nil
}

func (a *leaseTakeoverMidCheckpointAdapter) GetState() (map[string]any, error) {
	holder := "none"
	if pid := history.LeaseHolder(a.session); pid != 0 {
		for n, p := range a.procs {
			if p.cmd.Process.Pid == pid {
				holder = n
			}
		}
		if holder == "none" {
			return nil, fmt.Errorf("lease held by unknown pid %d", pid)
		}
	}
	if holder != a.holder {
		return nil, fmt.Errorf("lease holder mismatch: adapter says %q, flock says %q", a.holder, holder)
	}
	return map[string]any{
		"holder": a.holder, "p1_dead": a.p1Dead, "p2_dead": a.p2Dead,
		"seq": a.seq, "has_tree": a.hasTree, "entry_written": a.entryWritten,
		"author": a.author, "pinned": a.pinned,
		"dangling_exists": a.danglingExists, "pin_lost": a.pinLost,
	}, nil
}

func (a *leaseTakeoverMidCheckpointAdapter) want(name, msg string) error {
	got, err := a.procs[name].ask(msg)
	if err != nil {
		return err
	}
	if got != "ok" {
		return fmt.Errorf("%s %s: %s", name, msg, got)
	}
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) take(who string) error {
	if !a.gate.pass(a.holder == "none" && !(who == "p1" && a.p1Dead) && !(who == "p2" && a.p2Dead)) {
		return nil
	}
	if err := a.want(who, "take"); err != nil {
		return err
	}
	a.holder = who
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) crash(who string) error {
	if !a.gate.pass(a.holder == who) {
		return nil
	}
	a.kill(who)
	a.holder = "none"
	if who == "p1" {
		a.p1Dead = true
	} else {
		a.p2Dead = true
	}
	if a.author == who && a.entryWritten && !a.pinned && !a.dropPinLost {
		a.pinLost = true
	}
	return a.spawn(who)
}

func (a *leaseTakeoverMidCheckpointAdapter) restart(who string) error {
	dead := a.p1Dead
	if who == "p2" {
		dead = a.p2Dead
	}
	if !a.gate.pass(dead) {
		return nil
	}
	if who == "p1" {
		a.p1Dead = false
	} else {
		a.p2Dead = false
	}
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) snapshot(who string) error {
	dead := a.p1Dead
	if who == "p2" {
		dead = a.p2Dead
	}
	if !a.gate.pass(a.holder == who && !dead && !a.hasTree) {
		return nil
	}
	if err := a.want(who, "snapshot"); err != nil {
		return err
	}
	a.hasTree = true
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) appendEntry(who string) error {
	dead := a.p1Dead
	if who == "p2" {
		dead = a.p2Dead
	}
	if !a.gate.pass(a.holder == who && !dead && a.hasTree && !a.entryWritten) {
		return nil
	}
	if err := a.want(who, "append"); err != nil {
		return err
	}
	a.entryWritten = true
	a.author = who
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) pin(who string) error {
	dead := a.p1Dead
	if who == "p2" {
		dead = a.p2Dead
	}
	if !a.gate.pass(a.author == who && !dead && a.entryWritten && !a.pinned && !a.pinLost) {
		return nil
	}
	if err := a.want(who, "pin"); err != nil {
		return err
	}
	a.pinned = true
	return nil
}

func (a *leaseTakeoverMidCheckpointAdapter) P1Take() error     { return a.take("p1") }
func (a *leaseTakeoverMidCheckpointAdapter) P2Take() error     { return a.take("p2") }
func (a *leaseTakeoverMidCheckpointAdapter) P1Crash() error    { return a.crash("p1") }
func (a *leaseTakeoverMidCheckpointAdapter) P2Crash() error    { return a.crash("p2") }
func (a *leaseTakeoverMidCheckpointAdapter) P1Restart() error  { return a.restart("p1") }
func (a *leaseTakeoverMidCheckpointAdapter) P2Restart() error  { return a.restart("p2") }
func (a *leaseTakeoverMidCheckpointAdapter) P1Snapshot() error { return a.snapshot("p1") }
func (a *leaseTakeoverMidCheckpointAdapter) P2Snapshot() error { return a.snapshot("p2") }
func (a *leaseTakeoverMidCheckpointAdapter) P1Append() error   { return a.appendEntry("p1") }
func (a *leaseTakeoverMidCheckpointAdapter) P2Append() error   { return a.appendEntry("p2") }
func (a *leaseTakeoverMidCheckpointAdapter) P1Pin() error      { return a.pin("p1") }
func (a *leaseTakeoverMidCheckpointAdapter) P2Pin() error      { return a.pin("p2") }

func (a *leaseTakeoverMidCheckpointAdapter) NextTurn() error {
	if !a.gate.pass(a.entryWritten && a.seq < 2) {
		return nil
	}
	if !a.pinned {
		a.danglingExists = true
	}
	for _, n := range []string{"p1", "p2"} {
		if p := a.procs[n]; p != nil {
			if _, err := p.ask("forget"); err != nil {
				return err
			}
		}
	}
	a.seq++
	a.hasTree, a.entryWritten, a.pinned, a.pinLost = false, false, false, false
	a.author = "none"
	return nil
}

var leaseTakeoverMidCheckpointActions = map[string]map[string]fmbt.ActionFunc{"Checkpoint": {
	"P1Take":     action((*leaseTakeoverMidCheckpointAdapter).P1Take),
	"P2Take":     action((*leaseTakeoverMidCheckpointAdapter).P2Take),
	"P1Crash":    action((*leaseTakeoverMidCheckpointAdapter).P1Crash),
	"P2Crash":    action((*leaseTakeoverMidCheckpointAdapter).P2Crash),
	"P1Restart":  action((*leaseTakeoverMidCheckpointAdapter).P1Restart),
	"P2Restart":  action((*leaseTakeoverMidCheckpointAdapter).P2Restart),
	"P1Snapshot": action((*leaseTakeoverMidCheckpointAdapter).P1Snapshot),
	"P2Snapshot": action((*leaseTakeoverMidCheckpointAdapter).P2Snapshot),
	"P1Append":   action((*leaseTakeoverMidCheckpointAdapter).P1Append),
	"P2Append":   action((*leaseTakeoverMidCheckpointAdapter).P2Append),
	"P1Pin":      action((*leaseTakeoverMidCheckpointAdapter).P1Pin),
	"P2Pin":      action((*leaseTakeoverMidCheckpointAdapter).P2Pin),
	"NextTurn":   action((*leaseTakeoverMidCheckpointAdapter).NextTurn),
}}

func leaseTakeoverMidCheckpointOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 12, "max-parallel-runs": 0}
}

func TestLeaseTakeoverMidCheckpoint(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newLeaseTakeoverMidCheckpointAdapter(t)
	if err := runMBT(t, "lease_takeover_mid_checkpoint", a, leaseTakeoverMidCheckpointActions, leaseTakeoverMidCheckpointOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func TestLeaseTakeoverMidCheckpointCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newLeaseTakeoverMidCheckpointAdapter(t)
	a.dropPinLost = true
	if err := runMBT(t, "lease_takeover_mid_checkpoint", a, leaseTakeoverMidCheckpointActions, leaseTakeoverMidCheckpointOptions()); err == nil {
		t.Fatal("a run that never marks a dangling pin as lost passed; the runner is not checking state")
	}
}
