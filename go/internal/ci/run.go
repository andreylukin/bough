package ci

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"
)

// Options is one `bough ci` call.
type Options struct {
	Home   string   // whose ~/.bough holds the worktree and results
	Dir    string   // a directory inside the checkout
	Tree   string   // tree-ish; "" = the working tree now
	Checks []string // run exactly these (manual ones included); nil = every non-manual check
	NoWait bool     // report from the cache only; never run
	Rerun  bool     // ignore cached results for the selected checks
	// Progress gets one line per check started. Output goes to the log
	// only, so an agent's tool result stays a short report.
	Progress io.Writer
}

// States a check can be in.
const (
	StatePass    = "pass"
	StateFail    = "fail"
	StateUnknown = "unknown" // no result for this key, and nothing ran it
	StateRunning = "running" // no result yet, and another bough ci holds the lock
	StateManual  = "manual"  // not selected: runs only with --check
)

// CheckStatus is one check's line in the report.
type CheckStatus struct {
	Name  string `json:"name"`
	State string `json:"state"`
	// Cached is true when the result was not produced by this call.
	Cached bool    `json:"cached"`
	Key    string  `json:"key,omitempty"`
	Result *Result `json:"result,omitempty"`
}

// Report is what a call found.
type Report struct {
	Repo       string        `json:"repo"`
	Tree       string        `json:"tree"`
	ConfigFrom string        `json:"config_from"`
	Checks     []CheckStatus `json:"checks"`
}

// ExitCode is `bough ci`'s: 0 when every selected check passed, 1 when
// any failed (a failure is news even while others are pending), 2 when
// anything is unsettled. Manual checks that were not asked for do not
// count — but when they are all there is, nothing passed, and 0 would
// read as all green: that is 2 as well.
func (r Report) ExitCode() int {
	code := 0
	if r.NothingSelected() {
		return 2
	}
	for _, c := range r.Checks {
		switch c.State {
		case StateFail:
			return 1
		case StateUnknown, StateRunning:
			code = 2
		}
	}
	return code
}

// NothingSelected is true when every check in the report is manual.
func (r Report) NothingSelected() bool {
	for _, c := range r.Checks {
		if c.State != StateManual {
			return false
		}
	}
	return true
}

// prepared is the part of a call shared by Run and Log.
type prepared struct {
	store *Store
	tree  string
	files *treeFiles
	cfg   Config
	from  string
}

func prepare(ctx context.Context, o Options) (*prepared, error) {
	if o.Home == "" {
		return nil, errors.New("ci: no home directory")
	}
	r, err := Open(o.Dir)
	if err != nil {
		return nil, err
	}
	tree, err := r.Resolve(ctx, o.Tree)
	if err != nil {
		return nil, err
	}
	files, err := listTree(ctx, r, tree)
	if err != nil {
		return nil, err
	}
	cfg, from, err := LoadConfig(ctx, o.Home, r, files)
	if err != nil {
		return nil, err
	}
	return &prepared{store: &Store{Home: o.Home, Repo: r}, tree: tree, files: files, cfg: cfg, from: from}, nil
}

func (p *prepared) names() []string {
	var names []string
	for n := range p.cfg.Checks {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

func (p *prepared) unknownCheck(name string) error {
	return fmt.Errorf("ci: no check %q in %s (defined: %s)", name, p.from, strings.Join(p.names(), ", "))
}

// Run reports every selected check for the tree, running (unless
// NoWait) the ones with no stored result for their key.
func Run(ctx context.Context, o Options) (Report, error) {
	p, err := prepare(ctx, o)
	if err != nil {
		return Report{}, err
	}
	selected := map[string]bool{}
	for _, n := range o.Checks {
		if _, ok := p.cfg.Checks[n]; !ok {
			return Report{}, p.unknownCheck(n)
		}
		selected[n] = true
	}
	rep := Report{Repo: p.store.Repo.Top, Tree: p.tree, ConfigFrom: p.from}
	var pending []int // indexes into rep.Checks
	for _, n := range p.names() {
		ch := p.cfg.Checks[n]
		if len(ch.Inputs) > 0 && !p.files.anyMatch(ch.Inputs) {
			// Keyed on nothing, the check would pass once and be
			// cached for good, whatever later changed.
			return Report{}, fmt.Errorf("ci: check %q: inputs %q match no file in tree %s (a pattern without \"/\" matches only at the repo root; \"**\" spans directories)", n, ch.Inputs, short(p.tree))
		}
		st := CheckStatus{Name: n, Key: cacheKey(ch, p.files)}
		if len(o.Checks) > 0 && !selected[n] || len(o.Checks) == 0 && ch.Manual {
			st.State = StateManual
			if len(o.Checks) > 0 {
				continue // a subset was asked for; the rest is noise
			}
			rep.Checks = append(rep.Checks, st)
			continue
		}
		if !o.Rerun {
			res, err := p.store.Lookup(n, st.Key)
			if err != nil {
				return Report{}, err
			}
			if res != nil {
				st.State, st.Cached, st.Result = res.Status, true, res
			}
		}
		if st.State == "" {
			st.State = StateUnknown
			pending = append(pending, len(rep.Checks))
		}
		rep.Checks = append(rep.Checks, st)
	}
	if len(pending) == 0 {
		return rep, nil
	}
	state := p.store.dir()
	if err := os.MkdirAll(state, 0o755); err != nil {
		return Report{}, fmt.Errorf("ci: state dir: %w", err)
	}
	lockPath := filepath.Join(state, "lock")
	if o.NoWait {
		// Only to tell "someone is on it" from "nobody is": a free lock
		// is let go at once, and nothing runs.
		lf, held, err := lock(lockPath, false)
		if err != nil {
			return Report{}, fmt.Errorf("ci: lock %s: %w", lockPath, err)
		}
		if held {
			lf.Close()
		} else {
			for _, i := range pending {
				rep.Checks[i].State = StateRunning
			}
		}
		return rep, nil
	}
	// One lock per repo, held from moving the worktree through storing
	// the last result: every check runs in the one shared worktree.
	lf, held, err := lock(lockPath, false)
	if err == nil && !held {
		if o.Progress != nil {
			// Also what a check orphaned by a killed bough ci looks
			// like: it holds the lock until it ends (see inheritLock).
			fmt.Fprintf(o.Progress, "ci: waiting for another bough ci, or a check it left running (lock %s)…\n", lockPath)
		}
		lf, _, err = lock(lockPath, true)
	}
	if err != nil {
		return Report{}, fmt.Errorf("ci: lock %s: %w", lockPath, err)
	}
	defer lf.Close()
	for _, i := range pending {
		st := &rep.Checks[i]
		if !o.Rerun {
			// Whoever held the lock before us may have just run this.
			res, err := p.store.Lookup(st.Name, st.Key)
			if err != nil {
				return rep, err
			}
			if res != nil {
				st.State, st.Cached, st.Result = res.Status, true, res
				continue
			}
		}
		res, err := p.runOne(ctx, st.Name, st.Key, o.Progress, lf)
		if err != nil {
			return rep, err
		}
		st.State, st.Cached, st.Result = res.Status, false, res
	}
	return rep, nil
}

// runOne moves the worktree to the tree (again, per check: the previous
// check may have written into it) and runs the check there.
func (p *prepared) runOne(ctx context.Context, name, key string, progress io.Writer, lockFile *os.File) (*Result, error) {
	ch := p.cfg.Checks[name]
	if progress != nil {
		fmt.Fprintf(progress, "ci: running %s on %s…\n", name, short(p.tree))
	}
	if err := p.store.checkout(ctx, p.tree); err != nil {
		return nil, err
	}
	dir, _ := cleanDir(ch.Dir)
	res := Result{Check: name, Key: key, Tree: p.tree, Started: time.Now().UTC()}
	var log bytes.Buffer
	wd := filepath.Join(p.store.workDir(), filepath.FromSlash(dir))
	if fi, err := os.Stat(wd); err != nil || !fi.IsDir() {
		fmt.Fprintf(&log, "ci: check %s: dir %q is not a directory in tree %s\n", name, dir, short(p.tree))
		res.ExitCode = -1
	} else {
		c := exec.Command("sh", "-c", ch.Run)
		c.Dir = wd
		c.Env = append(cleanEnv(), "BOUGH_CI_TREE="+p.tree, "BOUGH_CI_CHECK="+name)
		c.Stdout, c.Stderr = &log, &log
		// A check that leaves a daemon holding stdout must not hang the
		// report: the pipes close this long after sh itself exits.
		c.WaitDelay = 2 * time.Second
		ownProcessGroup(c)
		inheritLock(c, lockFile)
		if err := c.Start(); err != nil {
			fmt.Fprintf(&log, "ci: check %s: start: %v\n", name, err)
			res.ExitCode = -1
		} else {
			done := make(chan error, 1)
			go func() { done <- c.Wait() }()
			select {
			case err = <-done:
				// Whatever sh left in the background would still be
				// writing when the next check moves the worktree.
				_ = killProcessGroup(c)
			case <-ctx.Done():
				_ = killProcessGroup(c)
				<-done
				// An interrupted run says nothing about the tree: it is
				// not stored, and the next call runs the check again.
				return nil, fmt.Errorf("ci: check %s interrupted: %w", name, ctx.Err())
			}
			if err != nil {
				res.ExitCode = -1
				if ee, ok := errors.AsType[*exec.ExitError](err); ok {
					res.ExitCode = ee.ExitCode()
					// Killed from outside — the OOM killer, a person, a
					// timeout that reached the check — says nothing
					// about the tree either, so it is not stored. sh
					// reports a killed child as 128+9.
					if ws, ok := ee.Sys().(syscall.WaitStatus); ok && ws.Signaled() || res.ExitCode == 128+int(syscall.SIGKILL) {
						return nil, fmt.Errorf("ci: check %s was killed (%s); nothing stored, run bough ci again", name, ee.ProcessState)
					}
				} else {
					fmt.Fprintf(&log, "ci: check %s: %v\n", name, err)
				}
			}
		}
	}
	res.Finished = time.Now().UTC()
	res.DurationMS = res.Finished.Sub(res.Started).Milliseconds()
	res.Status = StatePass
	if res.ExitCode != 0 {
		res.Status = StateFail
	}
	if err := p.store.save(res, log.Bytes()); err != nil {
		return nil, err
	}
	return &res, nil
}

// Log finds the stored log of check for the tree o names.
func Log(ctx context.Context, o Options, check string) (string, *Result, error) {
	p, err := prepare(ctx, o)
	if err != nil {
		return "", nil, err
	}
	ch, ok := p.cfg.Checks[check]
	if !ok {
		return "", nil, p.unknownCheck(check)
	}
	key := cacheKey(ch, p.files)
	res, err := p.store.Lookup(check, key)
	if err != nil {
		return "", nil, err
	}
	if res == nil {
		return "", nil, fmt.Errorf("ci: no result for %s at tree %s", check, short(p.tree))
	}
	return p.store.LogPath(check, key), res, nil
}
