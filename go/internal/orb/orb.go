package orb

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"maps"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
)

// Orb is one session's live container. Methods are safe for concurrent
// use: tools run bash and background jobs in parallel.
type Orb struct {
	rt      container.Runtime
	home    string
	session string
	project projectdef.Project
	scratch string

	mu    sync.Mutex
	state State
	spec  container.RunSpec
	proxy *proxy // host egress for the guest; nil when it could not start
	token string // proxy/relay token; "" for a container created before tokens

	secretWarned sync.Map // secret names already reported unresolved
}

// Open prepares a session's orb; see docs/orbs.md §1c.
func Open(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratchDir string) (*Orb, error) {
	if session == "" || strings.ContainsAny(session, `/\`) || session == "cache" || session == "images" {
		return nil, fmt.Errorf("orb: open: bad session id %q", session)
	}
	// Read before any writeState below overwrites it: the previous image
	// tells us whether an existing container is stale.
	prev, prevErr := ReadState(home, session)
	o := &Orb{rt: rt, home: home, session: session, project: p, scratch: scratchDir}
	o.state = State{Session: session, Project: p.Slug, Container: container.OrbName(session), PID: os.Getpid()}
	fail := func(err error) (*Orb, error) {
		o.state.Status, o.state.Error = StatusFailed, err.Error()
		writeState(home, o.state)
		return nil, fmt.Errorf("orb: open %s: %w", session, err)
	}
	if err := os.MkdirAll(Dir(home, session), 0o755); err != nil {
		return fail(err)
	}
	if err := rt.Available(ctx); err != nil {
		return fail(fmt.Errorf("%w (run `bough update` or `container system start`)", err))
	}
	if err := SyncRepos(ctx, home, p); err != nil {
		return fail(err)
	}

	o.state.Status = StatusBuilding
	writeState(home, o.state)
	tag, err := EnsureImage(ctx, rt, home, p, nil)
	if err != nil {
		return fail(err)
	}
	o.state.Image = tag
	o.state.Status = StatusStarting
	writeState(home, o.state)

	mounts, err := o.prepareMounts(ctx)
	if err != nil {
		return fail(err)
	}
	o.spec = container.RunSpec{
		Name: o.state.Container, Image: tag, Mounts: mounts,
		Env: o.baseEnv(), Workdir: o.state.Primary, CPUs: p.Def.CPUs, Memory: p.Def.Memory,
	}
	// An existing container is reused only when the last state we wrote
	// proves it runs this tag: a missing or unreadable state.json says
	// nothing about its image, mounts or env, and Start would silently
	// restart it as it was. A failed Remove fails the open for the same
	// reason.
	st, err := rt.Inspect(ctx, o.spec.Name)
	if err != nil {
		return fail(fmt.Errorf("inspect %s: %w", o.spec.Name, err))
	}
	if st != container.StateMissing && (prevErr != nil || prev.Image != tag) {
		if err := rt.Remove(ctx, o.spec.Name); err != nil {
			return fail(fmt.Errorf("remove stale %s: %w", o.spec.Name, err))
		}
		st = container.StateMissing
	}
	// A container's env is fixed at create: only a new one gets a new
	// token; a reused one keeps the token (or the lack of one) it has.
	if st == container.StateMissing {
		err = o.newTokenLocked()
	} else {
		o.token, err = readToken(home, session)
		o.setTokenEnvLocked()
	}
	if err != nil {
		return fail(fmt.Errorf("orb token: %w", err))
	}
	if err := rt.Start(ctx, o.spec); err != nil {
		// A concurrent session's build may have pruned our tag between
		// EnsureImage and Start (no container used it yet): rebuild once.
		if ok, ierr := rt.ImageExists(ctx, tag); ierr != nil || ok {
			return fail(err)
		}
		if tag, err = EnsureImage(ctx, rt, home, p, nil); err != nil {
			return fail(err)
		}
		o.state.Image, o.spec.Image = tag, tag
		writeState(home, o.state)
		if err := rt.Start(ctx, o.spec); err != nil {
			return fail(err)
		}
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	o.resumeLocked(ctx)
	return o, nil
}

// SyncRepos clones or fetches every remote repo's cache. Call it BEFORE
// hashing or building: a repo not cloned yet contributes no lockfiles,
// so the tag would be built without its dependencies.
func SyncRepos(ctx context.Context, home string, p projectdef.Project) error {
	for _, r := range p.Def.Repos {
		if r.Remote != "" {
			if err := syncCache(ctx, home, p.Slug, r); err != nil {
				return err
			}
		}
	}
	return nil
}

func (o *Orb) prepareMounts(ctx context.Context) ([]container.Mount, error) {
	p := o.project
	o.state.Worktrees = map[string]string{}
	var mounts []container.Mount
	seen := map[string]bool{}
	add := func(m container.Mount) {
		if !seen[m.Target] {
			seen[m.Target] = true
			mounts = append(mounts, m)
		}
	}
	for i, r := range p.Def.Repos {
		dst := filepath.Join(Dir(o.home, o.session), r.RepoName())
		if err := addWorktree(ctx, o.home, p.Slug, o.session, r, dst); err != nil {
			return nil, fmt.Errorf("worktree %s: %w", r.RepoName(), err)
		}
		o.state.Worktrees[r.RepoName()] = dst
		if i == 0 {
			o.state.Primary = dst
		}
		add(container.Mount{Source: dst, Target: dst})
		common, err := commonGitDir(ctx, dst)
		if err != nil {
			return nil, fmt.Errorf("worktree %s: %w", r.RepoName(), err)
		}
		add(container.Mount{Source: common, Target: common})
	}
	if o.scratch != "" {
		// The scratchpad makes its dir on first write, but a bind mount
		// needs the source now, and job scripts land there before any
		// scratch write.
		if err := os.MkdirAll(o.scratch, 0o755); err != nil {
			return nil, fmt.Errorf("scratch %s: %w", o.scratch, err)
		}
		add(container.Mount{Source: o.scratch, Target: o.scratch})
	}
	// Read-only so resume.sh is runnable in the guest; the agent edits the
	// definition through host tools, never from inside the container.
	add(container.Mount{Source: p.Dir, Target: p.Dir, ReadOnly: true})
	for _, dir := range p.Def.Caches {
		src := cacheDir(o.home, p.Slug, dir)
		if err := os.MkdirAll(src, 0o755); err != nil {
			return nil, fmt.Errorf("cache dir %s: %w", src, err)
		}
		add(container.Mount{Source: src, Target: dir})
	}
	for _, m := range identityMounts(o.home, p.Def.Identity) {
		add(m)
	}
	return mounts, nil
}

// cacheDir is a cache's host directory, named by its guest path so
// reordering caches never mounts one onto another dir. It is a bind, not
// a named volume: an Apple container volume is a block device that a
// second running VM cannot attach, and every session of the project
// shares this dir.
func cacheDir(home, slug, dir string) string {
	sum := sha256.Sum256([]byte(dir))
	return filepath.Join(home, ".bough", "cache", slug, hex.EncodeToString(sum[:])[:10])
}

// baseEnv is the container's own env; it holds no secrets, because run
// env is fixed for the container's life and visible to inspect.
func (o *Orb) baseEnv() []string {
	return append(o.coreEnv(), envList(o.project.Def.Env)...)
}

func (o *Orb) newTokenLocked() error {
	tok, err := newToken(o.home, o.session)
	if err != nil {
		return err
	}
	o.token = tok
	o.setTokenEnvLocked()
	if o.proxy != nil { // started without the token
		o.proxy.Close()
		o.proxy = nil
	}
	return nil
}

func (o *Orb) setTokenEnvLocked() {
	o.spec.Env = slices.DeleteFunc(o.spec.Env, func(e string) bool { return strings.HasPrefix(e, tokenEnv+"=") })
	if o.token != "" {
		o.spec.Env = append(o.spec.Env, tokenEnv+"="+o.token)
	}
	o.state.ProxyAuth = proxyAuth(o.token)
}

func (o *Orb) coreEnv() []string {
	env := []string{"HOME=/root", "TERM=dumb"}
	if o.scratch != "" {
		env = append(env, "BOUGH_SCRATCH="+o.scratch)
	}
	return env
}

// execEnv is passed on every exec because the engine does not inherit the
// host environment: the base env, the user's identity and the proxy.
// Project env comes last; projectdef refuses env names this sets. Secrets
// go separately in ExecOptions.Secrets, so they never reach argv.
func (o *Orb) execEnv(proxyURL, token string) []string {
	env := append(o.coreEnv(), identityEnv(o.project.Def.Identity)...)
	if token != "" {
		env = append(env, tokenEnv+"="+token)
	}
	if proxyURL != "" {
		env = append(env, proxyEnv(proxyURL)...)
		// The relay takes the token as a bearer header, not in the URL.
		env = append(env, "BOUGH_HOST="+strings.Replace(proxyURL, proxyUser+":"+token+"@", "", 1))
	}
	if o.scratch != "" {
		// The shim's dir first, so `bough` in the guest is the relay.
		env = append(env, "PATH="+shimDir(o.scratch)+":/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
	}
	return append(env, envList(o.project.Def.Env)...)
}

// secretEnv resolves the project's secrets, sorted by name. Refs are
// re-read from project.yml on every exec, so a secret added mid-session
// applies to the next command. An unresolved ref is left out and reported
// once per name.
func (o *Orb) secretEnv() []string {
	def := o.project.Def
	if p, err := projectdef.Load(o.home, o.project.Slug); err == nil {
		def = p.Def
	}
	var env []string
	for _, name := range slices.Sorted(maps.Keys(def.Secrets)) {
		val, err := secrets.Resolve(def.Secrets[name])
		if err != nil {
			if _, seen := o.secretWarned.LoadOrStore(name, true); !seen {
				fmt.Fprintf(os.Stderr, "bough: orb: secret %s unresolved: %v\n", name, err)
			}
			continue
		}
		env = append(env, name+"="+val)
	}
	return env
}

func (o *Orb) proxyURLLocked() string {
	if o.proxy == nil {
		return ""
	}
	return o.proxy.URL()
}

// ensureProxyLocked starts the host egress proxy on the guest's gateway
// address, read from the guest's resolv.conf (the engine's DNS forwarder
// lives on the gateway). A failure only costs internal-host reachability.
func (o *Orb) ensureProxyLocked(ctx context.Context) {
	if o.proxy != nil {
		return
	}
	out, err := o.rt.Command(ctx, o.spec.Name, container.ExecOptions{}, "sh", "-c", "awk '/^nameserver/{print $2; exit}' /etc/resolv.conf").Output()
	ip := net.ParseIP(strings.TrimSpace(string(out)))
	if err != nil || ip == nil || ip.IsLoopback() {
		return
	}
	p, err := startProxy(ip.String(), o.token)
	if err != nil {
		if o.rt.Name() != "fake" {
			fmt.Fprintf(os.Stderr, "bough: orb: proxy on %s: %v\n", ip, err)
		}
		return
	}
	o.proxy = p
}

// resumeLocked runs resume.sh and settles Running or Failed. A failing
// script leaves the container usable so the agent can fix it.
func (o *Orb) resumeLocked(ctx context.Context) {
	o.ensureProxyLocked(ctx)
	if o.scratch != "" {
		if err := writeShim(o.scratch); err != nil {
			fmt.Fprintf(os.Stderr, "bough: orb: bough shim: %v\n", err)
		}
	}
	o.state.Status, o.state.Error = StatusRunning, ""
	script := filepath.Join(o.project.Dir, projectdef.FileResume)
	if _, err := os.Stat(script); err == nil {
		if err := o.runResume(ctx, script); err != nil {
			o.state.Status, o.state.Error = StatusFailed, "resume.sh: "+err.Error()
		}
	}
	writeState(o.home, o.state)
}

func (o *Orb) runResume(ctx context.Context, script string) error {
	f, err := os.OpenFile(filepath.Join(Dir(o.home, o.session), "resume.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	text, _ := os.ReadFile(script)
	cmd := o.rt.Command(ctx, o.spec.Name, container.ExecOptions{Workdir: o.state.Primary, Env: o.execEnv(o.proxyURLLocked(), o.token), Secrets: o.secretEnv()}, container.ScriptArgv(text, script)...)
	cmd.Stdout, cmd.Stderr = f, f
	// Timestamps let session starts be measured without parsing output.
	start := time.Now()
	fmt.Fprintf(f, "== resume.sh start %s\n", start.UTC().Format(time.RFC3339))
	err = cmd.Run()
	status := "ok"
	if err != nil {
		status = err.Error()
	}
	fmt.Fprintf(f, "== resume.sh end %s duration %s: %s\n", time.Now().UTC().Format(time.RFC3339), time.Since(start).Round(time.Millisecond), status)
	return err
}

func (o *Orb) State() State {
	o.mu.Lock()
	defer o.mu.Unlock()
	s := o.state
	s.Worktrees = make(map[string]string, len(o.state.Worktrees))
	for k, v := range o.state.Worktrees {
		s.Worktrees[k] = v
	}
	return s
}

func (o *Orb) Root() string { return Dir(o.home, o.session) }

// Command is the exec seam. A container stopped behind the child's back
// (serve's Stop button, engine restart) is started again first.
func (o *Orb) Command(ctx context.Context, argv ...string) *exec.Cmd {
	o.mu.Lock()
	err := o.ensureRunningLocked(ctx)
	primary := o.state.Primary
	proxyURL := o.proxyURLLocked()
	token := o.token
	o.mu.Unlock()
	if err != nil {
		cmd := exec.CommandContext(ctx, "false")
		cmd.Err = fmt.Errorf("orb: %s: restart: %w", o.session, err)
		return cmd
	}
	cmd := o.rt.Command(ctx, o.spec.Name, container.ExecOptions{Workdir: primary, Env: o.execEnv(proxyURL, token), Secrets: o.secretEnv()}, argv...)
	// Killing the host `container exec` client does not end the guest
	// processes, so cancel also kills them inside the orb. A caller that
	// replaces Cancel (tools' process-group kill) must call this one too.
	if k, ok := o.rt.(guestKiller); ok {
		cmd.Cancel = k.KillFunc(o.spec.Name, cmd)
	}
	return cmd
}

// guestKiller is the runtime's optional guest-side kill (Apple.KillFunc).
type guestKiller interface {
	KillFunc(name string, cmd *exec.Cmd) func() error
}

func (o *Orb) ensureRunningLocked(ctx context.Context) error {
	st, err := o.rt.Inspect(ctx, o.spec.Name)
	if err != nil {
		return err
	}
	if st == container.StateRunning {
		return nil
	}
	if st == container.StateMissing {
		// Removed behind our back: Start creates it, so with a new token.
		if err := o.newTokenLocked(); err != nil {
			return err
		}
	}
	if err := o.rt.Start(ctx, o.spec); err != nil {
		o.state.Status, o.state.Error = StatusFailed, err.Error()
		writeState(o.home, o.state)
		return err
	}
	o.resumeLocked(ctx)
	return nil
}

// Stop keeps worktrees and the container so resume is fast.
func (o *Orb) Stop(ctx context.Context) error {
	o.mu.Lock()
	defer o.mu.Unlock()
	if err := o.rt.Stop(ctx, o.spec.Name); err != nil {
		return fmt.Errorf("orb: stop %s: %w", o.session, err)
	}
	if o.proxy != nil {
		o.proxy.Close()
		o.proxy = nil
	}
	o.state.Status = StatusStopped
	return writeState(o.home, o.state)
}

// Remove deletes the container, the session's worktrees and its orb dir.
// The bough/<session> branches are kept: they may hold unmerged work.
func Remove(ctx context.Context, rt container.Runtime, home, session string) error {
	if session == "" || strings.ContainsAny(session, `/\`) || session == "cache" || session == "images" {
		return fmt.Errorf("orb: remove: bad session id %q", session)
	}
	var errs []error
	if err := rt.Remove(ctx, container.OrbName(session)); err != nil {
		errs = append(errs, err)
	}
	dir := Dir(home, session)
	ents, _ := os.ReadDir(dir)
	for _, e := range ents {
		wt := filepath.Join(dir, e.Name())
		if fi, err := os.Stat(filepath.Join(wt, ".git")); err == nil && !fi.IsDir() {
			removeWorktree(ctx, wt)
		}
	}
	if err := os.RemoveAll(dir); err != nil {
		errs = append(errs, err)
	}
	if err := errors.Join(errs...); err != nil {
		return fmt.Errorf("orb: remove %s: %w", session, err)
	}
	return nil
}
