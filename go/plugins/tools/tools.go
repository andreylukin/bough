// Package tools is the "tools-basic" plugin: bash, view, patch
// registered into the codemode service. It also provides "turn-stats":
// the files written and the last bash exit code since the last Take,
// which the loop stamps onto its end-of-turn "done" entry.
package tools

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/hookmeta"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// bashTimeout is the tools.bash kill deadline (documented in the loop's
// system prompt; a var so tests can shorten it).
var bashTimeout = 60 * time.Second

// registry is the slice of the codemode service we need.
type registry interface {
	RegisterTool(name string, fn any)
}

// describer is codemode's optional prompt-catalogue seam: a tool
// documents itself where it is registered, so the model is never told
// about a tool that is not mounted (or left ignorant of one that is).
type describer interface {
	Describe(name, line string)
}

// runContexter is the optional slice of codemode that exposes the
// running script's context: the turn's cancel reaches tools.bash
// through it.
type runContexter interface {
	RunContext() context.Context
}

// orbExec is the "orb" service (plugins/orb) in a project session,
// declared here structurally so tools never imports another plugin.
type orbExec interface {
	// Command runs argv inside the session's container.
	Command(ctx context.Context, argv ...string) *exec.Cmd
	// Root is ~/.bough/orbs/<session>: every worktree lives under it.
	Root() string
}

// LocalPromptSection is the local session's read-only section. The orb
// row sets it, not this row: see internal/orb.LocalPromptSection.
const LocalPromptSection = iorb.LocalPromptSection

// errOrbNotReady is what a project session's bash and file tools say
// while the orb row is pending or failed: never fall back to the host.
var errOrbNotReady = errors.New("project orb not ready: see the orb row")

// pauser is codemode's seam for a tool that blocks longer than the
// script timeout (tools.ask uses it too); tools.jobWait needs it.
type pauser interface{ Pause() func() }

// Stats is the "turn-stats" service: side-effect tallies of the basic
// tools, reset by Take.
type Stats struct {
	runCtx func() context.Context // the running script's context; nil = none
	jobs   *Jobs                  // background jobs (never nil after Apply)
	// writeRoots, in a local session, are the only directories write and
	// patch exist for ($BOUGH_WRITE_ROOTS, set by a job bough starts, like
	// the wiki ingest). Empty: a local session has no write or patch.
	writeRoots []string

	mu    sync.Mutex
	files []string
	exit  int
	ran   bool // a bash call happened since the last Take
	runs  int  // bash calls ever made; never reset, so a caller can diff it
	// read remembers what each path looked like when it was last
	// viewed THIS turn, so a re-read that found nothing new can say so
	// (see view). Cleared by Take, like the rest of the turn's tally.
	read map[string]string
	// policy, when set, is asked before every bash command; its error
	// is the refusal the model sees (the rules row's Codex rules). ctx
	// is the command's: a policy that asks the person is released by
	// its end (Stop).
	policy func(ctx context.Context, cmd string) error
	// hadPolicy is set the first time a policy ever lands: a rules-row
	// reload disposes the old one (nil) before Apply installs the new
	// one, and a bash call landing in that gap must wait for whichever
	// policy comes next rather than run unchecked, permanently, as if
	// no rule had ever matched. A session that never mounts rules at
	// all never sets this, so its bash calls never pay the wait.
	hadPolicy bool
	// afterEdit, when set, is asked after every write and patch; what it
	// returns is appended to the tool's result (the lsp row's
	// diagnostics). bashNote does the same for a bash command's output.
	afterEdit func(path string) string
	bashNote  func(cmd string) string
	// project is set in a project session: bash runs through the orb
	// and write/patch stay inside its roots. nil = local/host.
	project *projectMode
	// callSink receives every foreground call's start and end (see
	// calls.go); nil = no per-call events. subWorker names the subagent
	// whose block is calling (0 = the parent), so the event is that
	// worker's "sub:call".
	callSink  callSink
	subWorker func() int
	calls     int // per-call event ids, never reset
}

// projectMode is a project session's routing: orb is resolved at call
// time because the orb row may mount after this one (Inject order).
type projectMode struct {
	slug string
	orb  func() (orbExec, error)
}

// command builds `sh script` on the host, or inside the orb in a
// project session; a missing orb is an error, never a host fallback.
func (p *projectMode) command(ctx context.Context, script string) (*exec.Cmd, error) {
	if p == nil {
		return exec.CommandContext(ctx, "sh", script), nil
	}
	o, err := p.orb()
	if err != nil {
		return nil, err
	}
	return o.Command(ctx, "sh", script), nil
}

// pause is the script-timeout pause, or nil when nothing runs scripts.
func (s *Stats) pause() func() func() {
	if s.jobs == nil {
		return nil
	}
	return s.jobs.pause
}

// turnCtx is the running script's context, or Background: what a
// codemode binding runs under. A native call brings its own instead.
func (s *Stats) turnCtx() context.Context {
	if s.runCtx != nil {
		if c := s.runCtx(); c != nil {
			return c
		}
	}
	return context.Background()
}

// pauseFor is the script-timeout pause for a call running under ctx:
// none for a native call, which has no script timer of its own, and
// pausing codemode's would stop an unrelated run_js block's clock.
func (s *Stats) pauseFor(ctx context.Context) func() func() {
	if isNative(ctx) {
		return nil
	}
	return s.pause()
}

// wait holds a project session's tool until its container is up. The
// orb row mounts before its container is running (the start runs on its
// own goroutine so the ui is not dead for an image build), and a turn
// can begin meanwhile; the script timeout is paused for the wait so a
// long build is not the block's failure. The turn's context bounds it.
func (p *projectMode) wait(ctx context.Context, pause func() func()) error {
	if p == nil {
		return nil
	}
	o, err := p.orb()
	if err != nil {
		return err
	}
	w, ok := o.(interface {
		Ready(context.Context) error
		Starting() bool
	})
	if !ok || !w.Starting() {
		return nil
	}
	if pause != nil {
		defer pause()()
	}
	return w.Ready(ctx)
}

// redactor is the orb's secret redactor; nil (pass through) on the host
// or for an orb that does not redact.
func (p *projectMode) redactor() *iorb.Redactor {
	if p == nil {
		return nil
	}
	o, err := p.orb()
	if err != nil {
		return nil
	}
	if r, ok := o.(interface{ Redactor() *iorb.Redactor }); ok {
		return r.Redactor()
	}
	return nil
}

// stoppedSince reports whether the orb was stopped under a job started at
// t; the host (nil) and an orb that cannot tell never were.
func (p *projectMode) stoppedSince(t time.Time) bool {
	if p == nil {
		return false
	}
	o, err := p.orb()
	if err != nil {
		return false
	}
	s, ok := o.(interface{ StoppedSince(time.Time) bool })
	return ok && s.StoppedSince(t)
}

// cancel kills c's host process group and, in a project session, also
// runs the orb's own Cancel: the host process there is only the
// `container exec` client, and killing it leaves the guest command
// (a dev server, a sleep) running in the orb.
func (p *projectMode) cancel(c *exec.Cmd) func() error {
	guest := c.Cancel
	return func() error {
		err := killProcessGroup(c)
		if p != nil && guest != nil {
			_ = guest()
		}
		return err
	}
}

// allowed refuses a write outside the orb's worktrees, the scratchpad
// and the session's own project definition: those are the only paths
// shared with (or meant for) the project.
func (p *projectMode) allowed(tool, path string) error {
	if p == nil {
		return nil
	}
	o, err := p.orb()
	if err != nil {
		return fmt.Errorf("%s: %w", tool, err)
	}
	roots := []string{o.Root()}
	if d := os.Getenv("BOUGH_SCRATCH"); d != "" {
		roots = append(roots, d)
	}
	if home, err := os.UserHomeDir(); err == nil && p.slug != "" {
		roots = append(roots, filepath.Join(home, ".bough", "projects", p.slug))
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return fmt.Errorf("%s: %w", tool, err)
	}
	// Symlinks resolve first: a link inside the worktree must not reach
	// a host file outside it.
	abs = resolveExisting(abs)
	for _, r := range roots {
		if rel, err := filepath.Rel(resolveExisting(filepath.Clean(r)), abs); err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return nil
		}
	}
	return fmt.Errorf("%s: %s is outside this project session; write under %s", tool, path, strings.Join(roots, ", "))
}

// canWrite confines write and patch: to the orb in a project session, to
// the write roots in a local session that has them. A local session
// without roots never registers either tool.
func (s *Stats) canWrite(ctx context.Context, tool, path string) error {
	if s.project != nil || len(s.writeRoots) == 0 {
		if s.project != nil {
			// The worktree the path must land in exists once the orb is up.
			if err := s.project.wait(ctx, s.pauseFor(ctx)); err != nil {
				return fmt.Errorf("%s: %w", tool, err)
			}
		}
		return s.project.allowed(tool, path)
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return fmt.Errorf("%s: %w", tool, err)
	}
	abs = resolveExisting(abs)
	for _, r := range s.writeRoots {
		if rel, err := filepath.Rel(resolveExisting(r), abs); err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return nil
		}
	}
	return fmt.Errorf("%s: %s is outside this session's writable directories; write under %s", tool, path, strings.Join(s.writeRoots, ", "))
}

// resolveExisting is EvalSymlinks on the longest existing prefix of an
// absolute path, so a file write is about to create still resolves
// through its parent's links.
func resolveExisting(abs string) string {
	rest := ""
	for p := abs; ; p = filepath.Dir(p) {
		if r, err := filepath.EvalSymlinks(p); err == nil {
			return filepath.Join(r, rest)
		}
		if filepath.Dir(p) == p {
			return abs
		}
		rest = filepath.Join(filepath.Base(p), rest)
	}
}

// SetPolicy installs (or, with nil, removes) the command policy.
func (s *Stats) SetPolicy(fn func(cmd string) error) {
	if fn == nil {
		s.SetPolicyContext(nil)
		return
	}
	s.SetPolicyContext(func(_ context.Context, cmd string) error { return fn(cmd) })
}

// SetPolicyContext is SetPolicy for a policy that takes the command's
// context: the block's run, or the native call's.
func (s *Stats) SetPolicyContext(fn func(ctx context.Context, cmd string) error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.policy = fn
	if fn != nil {
		s.hadPolicy = true
	}
}

// policyGrace bounds how long a bash call waits for a policy to
// reappear after a reload cleared it; a var so tests can shorten it.
var policyGrace = 3 * time.Second

// awaitPolicy polls for a policy to land, up to policyGrace or ctx's
// own cancellation, and gives up to nil (run unchecked) past either.
func (s *Stats) awaitPolicy(ctx context.Context) func(ctx context.Context, cmd string) error {
	deadline := time.Now().Add(policyGrace)
	for time.Now().Before(deadline) {
		s.mu.Lock()
		p := s.policy
		s.mu.Unlock()
		if p != nil {
			return p
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(10 * time.Millisecond):
		}
	}
	return nil
}

// SetAfterEdit sets (nil clears) the hook whose text follows every
// write and patch result.
func (s *Stats) SetAfterEdit(fn func(path string) string) {
	s.mu.Lock()
	s.afterEdit = fn
	s.mu.Unlock()
}

// SetBashNote sets (nil clears) the hook whose text follows a
// successful foreground bash command's output.
func (s *Stats) SetBashNote(fn func(cmd string) string) {
	s.mu.Lock()
	s.bashNote = fn
	s.mu.Unlock()
}

// edited is the afterEdit hook's text for path, or "".
func (s *Stats) edited(path string) string {
	s.mu.Lock()
	fn := s.afterEdit
	s.mu.Unlock()
	if fn == nil {
		return ""
	}
	if note := fn(path); note != "" {
		return "\n" + note
	}
	return ""
}

// Take returns the files written and the last bash exit code (ran is
// false when no bash call happened) since the previous Take, and resets.
func (s *Stats) Take() (files []string, exit int, ran bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	files, exit, ran = s.files, s.exit, s.ran
	s.files, s.exit, s.ran, s.read = nil, 0, false, nil
	return files, exit, ran
}

// wrote records a file this turn touched, once: a turn that edits the
// same file three times ended with "✔ wrote llm.go, llm.go, llm.go".
func (s *Stats) wrote(path string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if slices.Contains(s.files, path) {
		return
	}
	s.files = append(s.files, path)
}

func (s *Stats) exited(code int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.exit, s.ran = code, true
	s.runs++
}

// Bash reports how many bash calls have ever run and the last one's exit
// code, without resetting anything: the loop reads it before and after a
// block to stamp that block's own exit on its result, while Take keeps
// the turn's tally for the done entry.
func (s *Stats) Bash() (runs, exit int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.runs, s.exit
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
	st := &Stats{}
	// Mode is fixed before any row mounts, so reading it once here is
	// safe; absent (old tree, bare test context) means local.
	mode, _ := kernel.Get[string](ctx, "session-mode")
	local := mode != "project"
	if local {
		roots, err := kernel.Get[[]string](ctx, iorb.WriteRootsKey)
		if err != nil {
			roots = iorb.LocalWriteRoots() // a context without the launcher
		}
		st.writeRoots = append([]string(nil), roots...)
		// A local session assigned to a project may write that project's
		// directory. The context-md header names MEMORY.md by path every
		// turn, and "remember this" is the agent editing that file: without
		// the root the one write the prompt asks for is refused. A project
		// session already reaches it through projectMode.allowed.
		if dir, _ := kernel.Get[string](ctx, "session-project-dir"); dir != "" {
			st.writeRoots = append(st.writeRoots, filepath.Clean(dir))
		}
	}
	if !local {
		slug, _ := kernel.Get[string](ctx, "session-project")
		st.project = &projectMode{slug: slug, orb: func() (orbExec, error) {
			o, err := kernel.Get[orbExec](ctx, "orb")
			if err != nil {
				return nil, errOrbNotReady
			}
			return o, nil
		}}
	}
	if rc, ok := reg.(runContexter); ok {
		st.runCtx = rc.RunContext
	}
	// Per-call events: live to whoever renders the loop's events, and
	// the finished call into the session history (both resolved per
	// call: the history row remounts under /tree, workers mounts after
	// this row).
	st.callSink = func(kind, text string, data map[string]any, record bool) {
		if record {
			if rec, err := kernel.Get[func(string, map[string]any)](ctx, "history-record"); err == nil {
				entry := map[string]any{"text": text}
				maps.Copy(entry, data)
				rec(kind, entry)
			}
		}
		ctx.Emit("loop/event", loop.Event{Kind: kind, Text: text, Data: data})
	}
	st.subWorker = func() int {
		f, err := kernel.Get[func() int](ctx, "subagent-worker")
		if err != nil {
			return 0
		}
		return f()
	}
	// A background job outlives the turn that started it, so it hangs
	// off the plugin's context, not the script's.
	jctx, cancelJobs := context.WithCancel(context.Background())
	st.jobs = newJobs(jctx)
	// job_grace / job_settle: how long tools.bash(cmd, limit) waits in
	// the foreground before handing back a job, and how long jobs()/
	// job() wait for a change. "0s" makes every limited call a job at
	// once (the real-terminal suites want the pure background path).
	for _, k := range []string{"job_grace", "job_settle"} {
		v, ok := cfg[k]
		if !ok {
			continue
		}
		d, err := time.ParseDuration(fmt.Sprint(v))
		if err != nil || d < 0 {
			cancelJobs()
			return fmt.Errorf("tools-basic: %s must be a duration like 20s or 0s, got %v", k, v)
		}
		if k == "job_grace" {
			st.jobs.grace = d
		} else {
			st.jobs.settleFor = d
		}
	}
	st.jobs.runCtx = st.runCtx
	st.jobs.project = st.project
	st.jobs.stop = cancelJobs
	ctx.Effect(st.jobs.Stop)
	if p, ok := reg.(pauser); ok {
		st.jobs.pause = p.Pause
	}
	// Resolved at call time: /tree and /sessions remount history (and
	// the loop) under this row, which never remounts.
	st.jobs.owner = func() string {
		if h, err := kernel.Get[interface{ Path() string }](ctx, "history"); err == nil {
			return h.Path()
		}
		return ""
	}
	// Same provider row as "history", so no new remount edge; resolved
	// per write for the same reason owner is.
	st.jobs.record = func(kind string, data map[string]any) {
		if rec, err := kernel.Get[func(string, map[string]any)](ctx, "history-record"); err == nil {
			rec(kind, data)
		}
	}
	ctx.Provide("job-notices", st.jobs)
	if d, ok := reg.(describer); ok {
		for _, doc := range [][2]string{
			{"bash", `tools.bash(cmd) -> string: run a shell command, returns its output. Killed after 60 s (the error says so).`},
			{"bash-bg", `tools.bash(cmd, limit[, until]) -> string: the same command with a longer life — limit is seconds (or "10m"). It runs in the foreground for up to 20 s: if it ends by then you get its output right here, like a plain tools.bash. Only a command still running after that becomes a background job: you get its id, and you are told when it exits (or when its output matches the regexp until). Use it for a test suite, a build, a server you need up while you work. Never poll a job in a loop: tools.jobWait(id) blocks until it ends.`},
			{"jobs", `tools.jobs() -> string: the background jobs and their state (waits up to 10 s for a change first, so calling it in a loop is never the right move).`},
			{"job", `tools.job(id) -> string: one job's status and output so far (waits up to 10 s if it is still running).`},
			{"jobWait", `tools.jobWait(id, [seconds]) -> string: block until a job exits — the way to wait for one.`},
			{"jobKill", `tools.jobKill(id) -> string: stop a job.`},
			{"view", `tools.view(path, [start, end]) -> string: a file's lines, numbered ("12│text"); optional 1-based inclusive range. An image (png/jpg/gif/webp) is attached so you can see it.`},
			{"write", `tools.write(path, content) -> string: create or overwrite a whole file (use this for new files and rewrites, never a shell heredoc).`},
			{"patch", `tools.patch(path, old, new) -> string: replace ONE exact occurrence of old with new (copy old verbatim from view, enough lines to be unique).`},
		} {
			if local && len(st.writeRoots) == 0 && (doc[0] == "write" || doc[0] == "patch") {
				continue
			}
			d.Describe(doc[0], doc[1])
		}
	}
	reg.RegisterTool("bash", st.bash)
	reg.RegisterTool("jobs", st.jobs.jobs)
	reg.RegisterTool("job", st.jobs.job)
	reg.RegisterTool("jobWait", st.jobs.jobWait)
	reg.RegisterTool("jobKill", st.jobs.jobKill)
	// A person can stop a job too, not only the agent: serve's web view
	// sends "/jobkill N" down the child's stdin.
	if cmds, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "jobkill", Usage: "<id>", Summary: "stop a background job"}
		if err := cmds.Register(info, func(args string) (string, error) {
			id, err := strconv.Atoi(strings.TrimSpace(args))
			if err != nil {
				return "", fmt.Errorf("jobkill: want a job id, got %q", args)
			}
			return st.jobs.jobKill(id)
		}); err != nil {
			return err
		}
		ctx.Effect(func() { cmds.Unregister("jobkill") })
	}
	reg.RegisterTool("view", st.view)
	if !local || len(st.writeRoots) > 0 {
		reg.RegisterTool("patch", st.patch)
		reg.RegisterTool("write", st.write)
	}
	// The same tools for an engine that calls them natively; inert
	// under the loop, which never reads the registry.
	if at, err := kernel.Get[agenttools.Registry](ctx, "agent-tools"); err == nil {
		off, err := agenttools.RegisterAll(at, st.nativeTools(!local || len(st.writeRoots) > 0)...)
		if err != nil {
			return fmt.Errorf("tools-basic: %w", err)
		}
		ctx.Effect(off)
	}
	ctx.Provide("turn-stats", st)
	return nil
}

// bash runs cmd. With no limit it runs in the foreground and is killed
// after bashTimeout; given a limit (seconds, or a duration string) it
// becomes a background job that outlives the turn, and an optional
// third argument is a regexp to watch its output for.
// bash is bashRun with a per-call event around a foreground command
// (a background one is a job, with rows of its own).
func (s *Stats) bash(cmd string, opts ...any) (string, error) {
	if len(opts) > 0 && opts[0] != nil {
		return s.bashRun(cmd, opts...)
	}
	done := s.call("bash", firstLine(cmd))
	runsBefore, _ := s.Bash()
	out, err := s.bashRun(cmd, opts...)
	extra := map[string]any{}
	if runs, exit := s.Bash(); runs > runsBefore {
		extra["exit"] = exit
	}
	done(err, extra)
	return out, err
}

func (s *Stats) bashRun(cmd string, opts ...any) (string, error) {
	parent := context.Background()
	if s.runCtx != nil {
		parent = s.runCtx()
	}
	s.mu.Lock()
	policy := s.policy
	had := s.hadPolicy
	s.mu.Unlock()
	if policy == nil && had {
		policy = s.awaitPolicy(parent)
	}
	if policy != nil {
		if err := policy(parent, cmd); err != nil {
			return "", err
		}
	}
	project := s.project
	if hookmeta.OnHost(parent) {
		project = nil // a hook is the user's script: it runs on the host
	}
	// The container first, then any clock: a build still running is
	// neither the command's 60 seconds nor a job's limit.
	if err := project.wait(parent, s.pause()); err != nil {
		return "", fmt.Errorf("bash: %w", err)
	}
	if len(opts) > 0 && opts[0] != nil {
		limit, err := jobLimit(opts[0])
		if err != nil {
			return "", err
		}
		until := ""
		if len(opts) > 1 {
			if u, ok := opts[1].(string); ok {
				until = u
			}
		}
		b, err := s.jobs.start(cmd, limit, until)
		if err != nil {
			return "", err
		}
		// Most "background" commands end in seconds: answer those here,
		// as a foreground command would, and only hand back a job id
		// for one still running after the grace.
		if until == "" {
			if out, exit, jerr, finished := s.jobs.claim(b); finished {
				s.exited(exit)
				if jerr != "" {
					return "", fmt.Errorf("bash: job %d %s: %s%s", b.id, jerr, firstLine(cmd), tail(out))
				}
				return fmt.Sprintf("job %d finished in %s (it ended within the wait, so here is its output):\n%s", b.id, b.elapsed().Round(100*time.Millisecond).String(), out), nil
			}
		}
		msg := fmt.Sprintf("job %d started in the background (limit %s): %s", b.id, limit, firstLine(cmd))
		if until != "" {
			msg += fmt.Sprintf("\nwatching its output for %q", until)
		}
		return msg + "\nYou will be told when it finishes; tools.job(" + strconv.Itoa(b.id) + ") reads it meanwhile.", nil
	}
	ctx, cancel := context.WithTimeout(parent, bashTimeout)
	defer cancel()
	c, cleanup, err := project.shell(ctx, cmd)
	if err != nil {
		return "", err
	}
	defer cleanup()
	out, err := c.CombinedOutput()
	if r := project.redactor(); r != nil {
		out = []byte(r.String(string(out)))
	}
	// ErrWaitDelay means sh exited 0 but a backgrounded child (`cmd &`)
	// still holds the output pipe: the command succeeded, the child
	// keeps running, and what was written so far is the output.
	if errors.Is(err, exec.ErrWaitDelay) {
		err = nil
	}
	if ctx.Err() == context.DeadlineExceeded {
		s.exited(-1)
		return "", fmt.Errorf("bash: killed after %s: %s\n%s", bashTimeout, cmd, out)
	}
	if ctx.Err() == context.Canceled {
		s.exited(-1)
		return "", fmt.Errorf("bash: cancelled: %s\n%s", cmd, out)
	}
	if err != nil {
		code := -1
		if ee, ok := errors.AsType[*exec.ExitError](err); ok {
			code = ee.ExitCode()
		}
		s.exited(code)
		// "bash: exit status 1" alone is the header a collapsed error
		// row shows, and it says nothing. Lead with what failed and
		// what it said.
		return "", fmt.Errorf("bash: %s: %v%s", firstLine(cmd), err, tail(string(out)))
	}
	s.exited(0)
	s.mu.Lock()
	note := s.bashNote
	s.mu.Unlock()
	if note != nil {
		if n := note(cmd); n != "" {
			return string(out) + "\n" + n, nil
		}
	}
	return string(out), nil
}

// tail is a command's output for an error message: its first line on
// the header line (where a collapsed row can show it), then the rest.
func tail(out string) string {
	out = strings.TrimRight(out, "\n")
	if out == "" {
		return ""
	}
	head, rest, _ := strings.Cut(out, "\n")
	if rest == "" {
		return " — " + head
	}
	return " — " + head + "\n" + rest
}

// write creates or overwrites path with content, making parent
// directories. The plain way to put a whole file down: no heredoc
// quoting, no shell at all.
func (s *Stats) write(path, content string) (string, error) {
	done := s.call("write", path)
	_, statErr := os.Stat(path)
	out, err := s.writeFile(s.turnCtx(), path, content)
	extra := map[string]any{}
	if err == nil {
		extra["add"], extra["del"] = diffCounts(out)
		if statErr != nil { // a new file: every line is added, and there is no diff to count
			extra["add"] = lineCount(content)
		}
	}
	done(err, extra)
	return out, err
}

func (s *Stats) writeFile(ctx context.Context, path, content string) (string, error) {
	if err := s.canWrite(ctx, "write", path); err != nil {
		return "", err
	}
	before, hadFile := os.ReadFile(path)
	if dir := filepath.Dir(path); dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return "", err
		}
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		return "", err
	}
	s.wrote(path)
	out := fmt.Sprintf("wrote %s (%d bytes, %d lines)", path, len(content), lineCount(content))
	if hadFile == nil {
		out += lineDiff(string(before), content)
	}
	return out + s.edited(path), nil
}

// lineCount counts lines the way wc -l plus an unterminated tail does:
// "" is 0, "a\nb\n" is 2, "a\nb" is 2.
func lineCount(s string) int {
	n := strings.Count(s, "\n")
	if s != "" && !strings.HasSuffix(s, "\n") {
		n++
	}
	return n
}

// diffLimit caps the lines a diff considers; past it the change is
// reported without one (a rewrite of a big file is the code block).
const diffLimit = 400

// lineDiff renders old → new as "\n-old\n+new" lines, a blank line
// after the summary: an LCS over lines, unchanged lines omitted
// except one line of context on each side of a change, "…" for a
// gap between shown lines. "" when the
// texts are equal or either exceeds diffLimit lines. Pure.
func lineDiff(old, new string) string {
	if old == new {
		return ""
	}
	a, b := splitLines(old), splitLines(new)
	if len(a) > diffLimit || len(b) > diffLimit {
		return ""
	}
	// lcs[i][j] = LCS length of a[i:], b[j:].
	lcs := make([][]int, len(a)+1)
	for i := range lcs {
		lcs[i] = make([]int, len(b)+1)
	}
	for i, v := range slices.Backward(a) {
		for j := len(b) - 1; j >= 0; j-- {
			if v == b[j] {
				lcs[i][j] = lcs[i+1][j+1] + 1
			} else {
				lcs[i][j] = max(lcs[i+1][j], lcs[i][j+1])
			}
		}
	}
	type op struct {
		tag  byte
		text string
	}
	var ops []op
	for i, j := 0, 0; i < len(a) || j < len(b); {
		switch {
		case i < len(a) && j < len(b) && a[i] == b[j]:
			ops = append(ops, op{' ', a[i]})
			i++
			j++
		case i < len(a) && (j == len(b) || lcs[i+1][j] >= lcs[i][j+1]):
			ops = append(ops, op{'-', a[i]})
			i++
		default:
			ops = append(ops, op{'+', b[j]})
			j++
		}
	}
	var sb strings.Builder
	sb.WriteString("\n")
	skipped, shown := false, false
	for k, o := range ops {
		if o.tag == ' ' {
			near := (k > 0 && ops[k-1].tag != ' ') || (k+1 < len(ops) && ops[k+1].tag != ' ')
			if !near {
				skipped = true
				continue
			}
		}
		if skipped && shown {
			sb.WriteString("\n…")
		}
		skipped, shown = false, true
		sb.WriteString("\n" + string(o.tag) + o.text)
	}
	return sb.String()
}

// splitLines splits on newlines without a trailing empty element.
func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	return strings.Split(strings.TrimSuffix(s, "\n"), "\n")
}

// unchangedNote is appended when a view returns exactly what the same
// view returned earlier in the same turn.
//
// A real run read one file nineteen times in thirty-eight steps and
// nothing told it so. Re-reading after an edit is normal and this stays
// quiet for it — the note appears only when the bytes are identical,
// which means the step bought nothing.
const unchangedNote = "\n[you already read this in this turn and it has not changed since — it will not change unless something writes to it]"

// view is Stats.view: the read, plus the note when it repeats.
func (s *Stats) view(path string, rng ...int) (string, error) {
	done := s.call("view", viewDetail(path, rng))
	out, err := s.viewFile(path, rng...)
	done(err, nil)
	return out, err
}

// ReadAllowed is the project session's read rule for a reader outside
// this row: the engine's view_image, a harness op that never passes
// through viewFile. The engine finds it on turn-stats.
func (s *Stats) ReadAllowed(path string) error {
	return s.project.allowed("view_image", path)
}

func (s *Stats) viewFile(path string, rng ...int) (string, error) {
	// A project session reads only what it may write: the host outside
	// the orb (a loop's holdout among it) is not the project's.
	if err := s.project.allowed("view", path); err != nil {
		return "", err
	}
	out, err := readView(path, rng...)
	if err != nil {
		return out, err
	}
	key := path
	for _, n := range rng {
		key += ":" + strconv.Itoa(n)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.read == nil {
		s.read = map[string]string{}
	}
	if prev, seen := s.read[key]; seen && prev == out {
		return out + unchangedNote, nil
	}
	s.read[key] = out
	return out, nil
}

// readView returns a file's lines numbered "N│text", optionally only
// lines start..end (1-based, inclusive; end 0 = to the end). Numbers
// make patch targets and error lines easy to refer to.
func readView(path string, rng ...int) (string, error) {
	if st, serr := os.Stat(path); serr == nil && st.IsDir() {
		ents, rerr := os.ReadDir(path)
		if rerr != nil {
			return "", rerr
		}
		names := make([]string, 0, len(ents))
		for _, e := range ents {
			n := e.Name()
			if e.IsDir() {
				n += "/"
			}
			names = append(names, n)
		}
		return fmt.Sprintf("%s is a directory: %s", path, strings.Join(names, " ")), nil
	}
	// An image comes back as a marker the loop attaches as pixels.
	if llm.ImageMIME(path) != "" {
		if _, err := os.Stat(path); err != nil {
			return "", withNeighbours(path, err)
		}
		abs, _ := filepath.Abs(path)
		return "[Image: " + abs + "]", nil
	}
	f, err := os.Open(path)
	if err != nil {
		return "", withNeighbours(path, err)
	}
	defer f.Close()
	r := bufio.NewReader(f)
	// A NUL in the head marks a binary file (the usual VCS heuristic):
	// numbering its bytes as lines only feeds the model garbage.
	if head, _ := r.Peek(8000); bytes.IndexByte(head, 0) >= 0 {
		var sz int64
		if fi, err := f.Stat(); err == nil {
			sz = fi.Size()
		}
		return "", fmt.Errorf("view: %s is a binary file (%d bytes); inspect it with tools.bash (file, xxd, strings)", path, sz)
	}
	start, stop := 1, 0
	if len(rng) > 0 && rng[0] > 0 {
		start = rng[0]
	}
	if len(rng) > 1 && rng[1] > 0 {
		stop = rng[1]
	}
	// Stream: only the requested lines are kept, and never more than
	// viewCap bytes of them — a 50 MB log must not land in memory whole.
	var lines []string
	size, n, capped := 0, 0, false
	for {
		keep := 0
		if n+1 >= start && (stop == 0 || n+1 <= stop) {
			keep = viewCap - size + 1
		}
		line, full, rerr := readLineCapped(r, keep)
		if full == 0 && rerr != nil {
			break
		}
		n++
		if keep > 0 {
			if size+len(line) > viewCap {
				capped = true
				if len(lines) == 0 { // one line alone overruns the cap: show its head
					lines = append(lines, line[:viewCap])
				}
				break
			}
			size += len(line)
			lines = append(lines, strings.TrimSuffix(line, "\n"))
		}
		if rerr != nil || (stop > 0 && n >= stop) {
			break
		}
	}
	if n == 0 {
		n, lines = 1, []string{""} // an empty file is one empty line
	}
	if start > n {
		return "", fmt.Errorf("view: %s has %d lines, start %d is past the end", path, n, start)
	}
	if len(rng) > 1 && rng[1] > 0 && rng[1] < start {
		return "", fmt.Errorf("view: end %d is before start %d", rng[1], start)
	}
	width := len(strconv.Itoa(start + len(lines) - 1))
	var b strings.Builder
	for i, l := range lines {
		fmt.Fprintf(&b, "%*d│%s\n", width, start+i, l)
	}
	if capped {
		fmt.Fprintf(&b, "[view stopped at %d KB after line %d; pass a range, e.g. view(path, %d, %d)]\n", viewCap>>10, start+len(lines)-1, start+len(lines), start+len(lines)+999)
	}
	return b.String(), nil
}

// readLineCapped reads one line (through '\n') but keeps at most keep
// bytes of it, so an unbroken multi-MB line is never held whole. full is
// the line's true length.
func readLineCapped(r *bufio.Reader, keep int) (string, int, error) {
	var b []byte
	full := 0
	for {
		chunk, err := r.ReadSlice('\n')
		full += len(chunk)
		if room := keep - len(b); room > 0 {
			b = append(b, chunk[:min(room, len(chunk))]...)
		}
		if err != bufio.ErrBufferFull {
			return string(b), full, err
		}
	}
}

// viewCap bounds the bytes of file text one view returns.
const viewCap = 256 << 10

// withNeighbours turns a bare "no such file" into one that names the
// files actually next to the guessed path: a model that invents
// go/plugins/loop/turn.go should be told loop.go and cancel.go exist,
// not left to guess again.
func withNeighbours(path string, err error) error {
	if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	dir := filepath.Dir(path)
	ents, rerr := os.ReadDir(dir)
	if rerr != nil {
		return fmt.Errorf("%w (no directory %s either)", err, dir)
	}
	base := strings.ToLower(strings.TrimSuffix(filepath.Base(path), filepath.Ext(path)))
	var near, all []string
	for _, e := range ents {
		n := e.Name()
		if e.IsDir() {
			n += "/"
		}
		all = append(all, n)
		if l := strings.ToLower(n); strings.Contains(l, base) || strings.Contains(base, strings.TrimSuffix(l, filepath.Ext(l))) {
			near = append(near, n)
		}
	}
	list := near
	if len(list) == 0 {
		list = all
	}
	if len(list) > 12 {
		list = append(list[:12:12], "…")
	}
	if len(list) == 0 {
		return fmt.Errorf("%w (%s is empty)", err, dir)
	}
	return fmt.Errorf("%w — %s holds: %s", err, dir, strings.Join(list, " "))
}

// closestMatch returns the 1-based line of the run of lines (as many
// as old spans) in data most similar to old, and whether the file has
// one: a model that mistyped its old text or copied it from an older
// version of the file should be shown what is nearly there, not left
// to view the whole file and guess again. Ties go to the earliest
// window. Pure.
func closestMatch(data, old string) (int, bool) {
	if old == "" {
		return 0, false
	}
	ls := splitLines(data)
	n := strings.Count(old, "\n") + 1
	if len(ls) < n {
		return 0, false
	}
	best, bestDist := 0, -1
	for i := 0; i+n <= len(ls); i++ {
		w := strings.Join(ls[i:i+n], "\n")
		// |len(a)-len(b)| is a lower bound on the distance: skip
		// windows that cannot beat the best so far, so a long file
		// with a good early match stays cheap.
		gap := len(w) - len(old)
		if gap < 0 {
			gap = -gap
		}
		if bestDist >= 0 && gap >= bestDist {
			continue
		}
		if d := editDistance(old, w); bestDist < 0 || d < bestDist {
			best, bestDist = i+n/2+1, d
		}
	}
	return best, true
}

// editDistance is the Levenshtein distance between a and b: how many
// single-character insertions, deletions or replacements turn a into
// b. Pure.
func editDistance(a, b string) int {
	ar, br := []rune(a), []rune(b)
	prev := make([]int, len(br)+1)
	cur := make([]int, len(br)+1)
	for j := range prev {
		prev[j] = j
	}
	for i := 1; i <= len(ar); i++ {
		cur[0] = i
		for j := 1; j <= len(br); j++ {
			cost := 1
			if ar[i-1] == br[j-1] {
				cost = 0
			}
			cur[j] = min(min(cur[j-1]+1, prev[j]+1), prev[j-1]+cost)
		}
		prev, cur = cur, prev
	}
	return prev[len(br)]
}

// nearestLines renders the lines of data around line at (1-based),
// lines on each side, numbered "N│text" like view: the neighbourhood
// the model should have copied old from. "" when at is outside the
// file.
func nearestLines(data string, at, lines int) string {
	ls := splitLines(data)
	if at < 1 || at > len(ls) {
		return ""
	}
	lo, hi := max(at-lines, 1), min(at+lines, len(ls))
	width := len(strconv.Itoa(hi))
	var b strings.Builder
	for n := lo; n <= hi; n++ {
		fmt.Fprintf(&b, "%*d│%s\n", width, n, ls[n-1])
	}
	return b.String()
}

// pathLocks serialises patches to one file across all agents in the
// process; entries are never freed (one small mutex per patched path).
var pathLocks sync.Map

func lockPath(path string) func() {
	if abs, err := filepath.Abs(path); err == nil {
		path = abs
	}
	m, _ := pathLocks.LoadOrStore(path, &sync.Mutex{})
	mu := m.(*sync.Mutex)
	mu.Lock()
	return mu.Unlock
}

// patch replaces one exact occurrence of old with new in path. old
// must match exactly once (include more context when it repeats). An
// empty old creates the file with new when it does not exist yet.
func (s *Stats) patch(path, old, new string) (string, error) {
	done := s.call("patch", path)
	out, err := s.patchFile(s.turnCtx(), path, old, new)
	extra := map[string]any{}
	if err == nil {
		extra["add"], extra["del"] = diffCounts(out)
	}
	done(err, extra)
	return out, err
}

func (s *Stats) patchFile(ctx context.Context, path, old, new string) (string, error) {
	if err := s.canWrite(ctx, "patch", path); err != nil {
		return "", err
	}
	// Every Stats (one per agent) shares this lock, so the
	// read-modify-write below never interleaves with another patch.
	unlock := lockPath(path)
	defer unlock()
	data, err := os.ReadFile(path)
	if err != nil && old != "" {
		err = withNeighbours(path, err)
	}
	if old == "" {
		if err == nil {
			return "", fmt.Errorf("patch: %s exists; give the text to replace (old) or create a new path", path)
		}
		if !errors.Is(err, os.ErrNotExist) {
			return "", err
		}
		if dir := filepath.Dir(path); dir != "." {
			if err := os.MkdirAll(dir, 0o755); err != nil {
				return "", err
			}
		}
		if err := os.WriteFile(path, []byte(new), 0o644); err != nil {
			return "", err
		}
		s.wrote(path)
		return fmt.Sprintf("created %s (%d bytes)", path, len(new)), nil
	}
	if err != nil {
		return "", err
	}
	// view hides \r, so an LF old copied from a CRLF file never matches
	// byte-for-byte: translate old and new to the file's line endings.
	if !strings.Contains(string(data), old) && strings.Contains(old, "\n") &&
		!strings.Contains(old, "\r") && strings.Contains(string(data), "\r\n") {
		old = strings.ReplaceAll(old, "\n", "\r\n")
		new = strings.ReplaceAll(strings.ReplaceAll(new, "\r\n", "\n"), "\n", "\r\n")
	}
	switch n := strings.Count(string(data), old); n {
	case 0:
		if line, ok := closestMatch(string(data), old); ok {
			if near := nearestLines(string(data), line, 2); near != "" {
				return "", fmt.Errorf("patch: old text not found in %s (view it and copy the exact lines) — closest match near line %d:\n%s", path, line, near)
			}
		}
		return "", fmt.Errorf("patch: old text not found in %s (view it and copy the exact lines)", path)
	case 1:
	default:
		return "", fmt.Errorf("patch: old text occurs %d times in %s; include more surrounding lines", n, path)
	}
	if old == new {
		return "", fmt.Errorf("patch: old and new text are identical — no change to %s", path)
	}
	out := strings.Replace(string(data), old, new, 1)
	if err := os.WriteFile(path, []byte(out), 0o644); err != nil {
		return "", err
	}
	s.wrote(path)
	return fmt.Sprintf("patched %s (%+d lines)", path,
		strings.Count(new, "\n")-strings.Count(old, "\n")) + lineDiff(old, new) + s.edited(path), nil
}

// shell builds the process for a foreground command: `sh script`, on
// the host or in the orb, in its own process group. cleanup removes the
// script once the command is done.
func (p *projectMode) shell(ctx context.Context, cmd string) (c *exec.Cmd, cleanup func(), err error) {
	// The script goes in a file, not as an argument: a heredoc'd file
	// or a long one-liner is not bounded by ARG_MAX, and a stray NUL
	// byte no longer makes exec fail with "invalid argument". Not on
	// stdin either: a stdin reader (cat, read, ssh) would eat the rest
	// of the script. stdin is /dev/null.
	// $BOUGH_SCRATCH (the scratchpad row) is promised to the command
	// as a usable directory; it is made lazily and may have been
	// removed since, so make sure it exists before the command runs.
	if d := os.Getenv("BOUGH_SCRATCH"); d != "" {
		_ = os.MkdirAll(d, 0o755)
	}
	script, err := p.script(cmd)
	if err != nil {
		return nil, nil, err
	}
	c, err = p.command(ctx, script)
	if err != nil {
		os.Remove(script)
		return nil, nil, fmt.Errorf("bash: %w", err)
	}
	// Its own process group, killed as a group: `sh -c` execs or forks
	// the command, and killing sh alone leaves a sleep, a server, a
	// build running after the turn was cancelled.
	ownProcessGroup(c)
	c.Cancel = p.cancel(c)
	c.WaitDelay = 2 * time.Second
	return c, func() { os.Remove(script) }, nil
}

// script writes cmd for `sh <file>`. In a project session the file goes
// in $BOUGH_SCRATCH, which the container mounts at the same path; the
// host temp dir is invisible inside it.
func (p *projectMode) script(cmd string) (string, error) {
	if p == nil {
		return bashScript(cmd)
	}
	return bashScriptIn(os.Getenv("BOUGH_SCRATCH"), cmd)
}

// bashScript writes cmd to a temp file for `sh <file>`; the caller removes it.
func bashScript(cmd string) (string, error) { return bashScriptIn("", cmd) }

func bashScriptIn(dir, cmd string) (string, error) {
	f, err := os.CreateTemp(dir, "bough-bash-*.sh")
	if err != nil {
		return "", fmt.Errorf("bash: script file: %v", err)
	}
	_, err = f.WriteString(cmd)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		os.Remove(f.Name())
		return "", fmt.Errorf("bash: script file: %v", err)
	}
	return f.Name(), nil
}
