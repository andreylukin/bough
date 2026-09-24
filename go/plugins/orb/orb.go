// Package orb is the "orb" plugin: a project session's container. It
// opens the session's orb (worktrees, image, running container), moves
// the process into the primary worktree, and provides the "orb" exec
// seam tools.bash runs through plus "orb-state" for the ui. A local
// session mounts nothing. See docs/orbs.md §2.
package orb

import (
	"bytes"
	"context"
	_ "embed"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/loop"
	"github.com/andreylukin/bough/plugins/tools"
)

// chdir is swapped by tests: os.Chdir is process-global and would race
// every other parallel test.
var chdir = os.Chdir

// userHome is swapped by the mount test for the same reason as chdir:
// t.Setenv("HOME") cannot run in parallel.
var userHome = os.UserHomeDir

type pather interface{ Path() string }
type direr interface{ Dir() string }
type sections interface{ Set(name, text string) }

type plugin struct{}

// opened holds each session's orb across reloads of this row. The row
// reloads whenever a service it read lands late (the loop's
// prompt-sections always does), and stopping and reopening the
// container then would kill background jobs and rerun resume.sh.
var opened = struct {
	sync.Mutex
	m map[string]openOrb
}{m: map[string]openOrb{}}

type openOrb struct {
	h    *handle
	slug string
	cfg  map[string]any
}

func init() {
	kernel.Register("orb", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string { return "orb" }

// Inject lists service keys: the session id comes from history and the
// scratch dir is mounted into the container at its host path.
func (plugin) Inject() []string { return []string{"history", "scratch"} }

// newFake is swapped by tests that need a hand on the fake runtime.
var newFake = func() container.Runtime { return container.NewFake() }

// RuntimeFor is the backend a `runtime:` value on the orb row names, for
// serve, which must ask about project containers through the same one
// its sessions run them on.
func RuntimeFor(name string) (container.Runtime, error) { return runtimeFor(name) }

// runtimeFor maps the row's runtime config to a backend.
func runtimeFor(name string) (container.Runtime, error) {
	switch name {
	case "":
		return container.Default(), nil
	case "apple":
		return container.NewApple(), nil
	case "nerdctl":
		return container.Nerdctl{}, nil
	case "podman":
		return container.NewPodman(), nil
	case "fake":
		return newFake(), nil
	}
	return nil, fmt.Errorf("orb: runtime %q: want apple, nerdctl, podman or fake", name)
}

// skillMD is the /orb skill: how to populate a project definition. Like
// llm-wiki it ships in the binary and is written to ~/.bough/skills on every
// run (both modes: a local session sets projects up too), so it matches the
// `bough project` CLI it describes.
//
//go:embed SKILL.md
var skillMD []byte

// InstallSkill writes the /orb skill under home, only when it changed.
func InstallSkill(home string) error {
	p := filepath.Join(home, ".bough", "skills", "orb", "SKILL.md")
	if b, err := os.ReadFile(p); err == nil && bytes.Equal(b, skillMD) {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return err
	}
	return os.WriteFile(p, skillMD, 0o644)
}

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		if k != "runtime" {
			return fmt.Errorf("orb: unknown config key %q", k)
		}
	}
	if home, err := os.UserHomeDir(); err == nil {
		if err := InstallSkill(home); err != nil {
			fmt.Fprintf(os.Stderr, "bough: orb: install /orb skill: %v\n", err)
		}
	}
	if mode, _ := kernel.Get[string](ctx, "session-mode"); mode != "project" {
		// The read-only note is set here, not by tools: this row provides
		// nothing in local mode, so its reload when the loop's sections
		// land is free, where a tools reload cascades into loop and ui.
		if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
			s.Set("mode", iorb.LocalPromptSectionFor(iorb.LocalWriteRoots()))
			ctx.Effect(func() { s.Set("mode", "") })
		}
		return nil
	}
	slug, _ := kernel.Get[string](ctx, "session-project")
	name, _ := cfg["runtime"].(string)
	rt, err := runtimeFor(name)
	if err != nil {
		return err
	}
	hist, err := kernel.Get[pather](ctx, "history")
	if err != nil {
		return fmt.Errorf("orb: %w", err)
	}
	pad, err := kernel.Get[direr](ctx, "scratch")
	if err != nil {
		return fmt.Errorf("orb: %w", err)
	}
	session := strings.TrimSuffix(filepath.Base(hist.Path()), ".jsonl")
	home, err := userHome()
	if err != nil {
		return fmt.Errorf("orb: home dir: %w", err)
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		return fmt.Errorf("orb: open %s: %w", slug, err)
	}
	// Open can build an image; bound it so a wedged engine fails the
	// start instead of hanging forever. The context lives as long as the
	// handle: the start runs past this Apply.
	octx, cancel := context.WithTimeout(context.Background(), startTimeout)
	if err := rt.Available(octx); err != nil {
		cancel()
		return fmt.Errorf("orb: open %s: runtime %s: %w (run `bough update` or `container system start`)", slug, rt.Name(), err)
	}
	opened.Lock()
	prev, reused := opened.m[session]
	delete(opened.m, session)
	opened.Unlock()
	reused = reused && prev.slug == slug && reflect.DeepEqual(prev.cfg, cfg)
	if !reused && prev.h != nil {
		prev.h.close()
	}
	h := prev.h
	if reused {
		cancel()
	} else {
		h = newHandle(home, session, slug, rt, pad.Dir(), cancel)
		h.rs = newRestarter(h, ctx)
		// Set before start: the goroutines read them from then on.
		h.rs.histPath, h.rs.headless = hist.Path(), headlessRun(ctx)
		h.rs.start()
		// The host half now: repos synced, worktrees added, the process
		// moved into the primary worktree. Seconds of local git, and
		// what the first turn's checkpoint, cwd and prompt need. The
		// container half runs on its own.
		o, err := prepare(octx, rt, home, session, p, pad.Dir())
		start := func() {
			if err != nil {
				h.settle(nil, err)
				return
			}
			if err := o.Start(octx); err != nil {
				h.settle(nil, err)
				return
			}
			h.settle(o, nil)
		}
		if headlessRun(ctx) {
			// A person or script ran this one turn: running it without
			// the orb would answer from nowhere, and exit 0 or a model
			// error hid the cause. So this one start is waited for, and
			// its failure is the row's.
			start()
			if err := h.Ready(octx); err != nil {
				h.close()
				st, _ := iorb.ReadState(home, session)
				return fmt.Errorf("%s", failureReport(slug, iorb.FailedAt(st), err))
			}
			// resume.sh failing leaves the container up but the project
			// broken (no deps, no dev server): no turn runs against it.
			if st := h.Orb().State(); st.Status == iorb.StatusFailed {
				h.close()
				return fmt.Errorf("%s", failureReport(slug, iorb.FailedAt(st), fmt.Errorf("%s", st.Error)))
			}
		} else {
			// Everyone else gets the row now and the orb when it is up:
			// the rows after this one (ui, web) mount at once, so the
			// person can type, pick a model and read while the image
			// builds. tools.bash, write and patch wait on the handle.
			// A failed start used to kill the whole session process
			// before it read stdin, and a message sent to it vanished
			// with no error anywhere; the session stays up without an
			// orb, says why, and the next start retries the build.
			go start()
		}
	}
	// Which session in the project this is. serve sets
	// BOUGH_PROJECT_MAIN on the main thread, BOUGH_PROJECT_THREAD on a
	// thread a person started and BOUGH_SPAWNED_BY on the threads main
	// starts; a session started from the CLI is none and is told none.
	isMain, _ := kernel.Get[bool](ctx, "session-main")
	isThread, _ := kernel.Get[bool](ctx, "session-thread")
	parent, _ := kernel.Get[string](ctx, "session-spawned-by")
	r := projectRole{main: isMain, thread: isThread, parent: parent}
	secs, _ := kernel.Get[sections](ctx, "prompt-sections")
	uiMode := uiModeOf(ctx)
	// A restart's outcome goes to the agent as a job notice, which wakes
	// an idle one: that is how it learns its jobs died, since jobs killed
	// by an orb stop deliberately send none. With no tools row, the person.
	if h.rs != nil {
		h.rs.setNotify(func(text string) {
			if n, err := kernel.Get[interface{ Notify(string) }](ctx, "job-notices"); err == nil {
				n.Notify(text)
				return
			}
			say(ctx, uiMode, text)
		})
		// The swap waits for the turn to end; the loop's events say when.
		// Subscriptions made in Apply are disposed with the row.
		ctx.On("loop/event", func(payload any) {
			if ev, ok := payload.(loop.Event); ok {
				h.rs.observe(ev.Kind)
			}
		})
	}
	// gone is this Apply's lifetime: a watcher from before a reload must
	// not write a section the reload's Effect just cleared.
	gone := make(chan struct{})
	// settled is the per-Apply work that needs the open orb: the prompt
	// section with its address, the notices, the address refresh on
	// restart. On a reused, already open orb it runs at once. A restart
	// runs it again, quiet (its notice says what changed), for the orb
	// now behind the handle: the definition is read again, so checks,
	// identity and ports come from the one the restart applied.
	settled := func(quiet bool) {
		select {
		case <-gone:
			return
		default:
		}
		loud := !reused && !quiet
		p := p
		if fresh, err := projectdef.Load(home, slug); err == nil {
			p = fresh
		}
		o := h.Orb()
		if o == nil {
			// Said once, by the Apply whose start it was: a reload keeps
			// the failed handle and only restores the section.
			err := h.Ready(context.Background())
			if loud {
				st, _ := iorb.ReadState(home, session)
				say(ctx, uiMode, failureReport(slug, iorb.FailedAt(st), err))
			}
			if secs != nil {
				secs.Set("orb", failedPromptSection(slug, err))
			}
			return
		}
		st := o.State()
		if st.ProxyAuth == iorb.ProxyAuthLegacy && loud {
			say(ctx, uiMode, fmt.Sprintf("orb: %s: %s", slug, iorb.LegacyProxyNotice))
		}
		resume, _ := projectdef.ReadFile(home, slug, projectdef.FileResume)
		setup, _ := projectdef.ReadFile(home, slug, projectdef.FileSetup)
		dockerfile, _ := projectdef.ReadFile(home, slug, projectdef.FileDockerfile)
		missing := missingEnv(resume, p.Def.Checks, p.Def, setup+"\n"+dockerfile)
		if len(missing) > 0 && loud {
			say(ctx, uiMode, fmt.Sprintf("orb: %s: unset env %s (resume.sh/checks)", slug, strings.Join(missing, ", ")))
		}
		if st.Status == iorb.StatusFailed && loud {
			say(ctx, uiMode, failureReport(slug, iorb.FailedAt(st), fmt.Errorf("%s", st.Error)))
		}
		if secs != nil {
			root := o.Root()
			memory := filepath.Join(p.Dir, projectdef.FileMemory)
			secs.Set("orb", promptSection(root, memory, st, p.Def, missing, r))
			// A restart gets a new IP: keep the prompt's address current.
			o.OnResume(func(st iorb.State) {
				select {
				case <-gone:
				default:
					secs.Set("orb", promptSection(root, memory, st, p.Def, missing, r))
				}
			})
		}
	}
	if h.Starting() {
		if secs != nil {
			secs.Set("orb", startingPromptSection(slug, h.Root(), r))
		}
		go func() {
			select {
			case <-h.ready:
				settled(false)
			case <-gone:
			}
		}()
	} else {
		settled(false)
	}
	h.setRefresh(settled)
	ctx.Effect(func() {
		close(gone)
		h.setRefresh(nil)
		h.OnResume(nil)
		if secs != nil {
			secs.Set("orb", "")
		}
	})
	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		registerOrbCommand(ctx, reg, h, home)
	}
	// Resolved secrets never reach history: every entry goes through the
	// orb's redactor (a no-op under `redact: false`, and before it is up).
	if r, ok := hist.(interface{ SetRedact(func(string) string) }); ok {
		r.SetRedact(h.Redact)
		ctx.Effect(func() { r.SetRedact(nil) })
	}
	registerPortalTools(ctx, home, session)
	ctx.Provide("orb", h)
	ctx.Provide("orb-state", h)
	// Stop, never Remove: a resumed session reuses its worktrees and
	// container. A reload (this row still mounted and still desired as
	// is) keeps the orb running for the next Apply instead.
	ctx.Effect(func() {
		if reloading(ctx, cfg) {
			opened.Lock()
			opened.m[session] = openOrb{h: h, slug: slug, cfg: cfg}
			opened.Unlock()
			return
		}
		h.close()
	})
	return nil
}

// prepare is the host half of a start: the orb's worktrees, then the
// process's move into the primary one (checkpoints and relative paths
// use the cwd, which is also the container's workdir).
func prepare(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratch string) (*iorb.Orb, error) {
	o, err := iorb.Prepare(ctx, rt, home, session, p, scratch)
	if err != nil {
		return nil, err
	}
	if st := o.State(); st.Primary != "" {
		if err := chdir(st.Primary); err != nil {
			err = fmt.Errorf("chdir %s: %w", st.Primary, err)
			o.Abort(err)
			return nil, fmt.Errorf("orb: %w", err)
		}
	}
	return o, nil
}

// reloading reports whether the orb row is being disposed only to be
// applied again: it is still mounted (Unmount clears that first) and a
// desired row still carries the same config (Reconcile swaps the
// desired set before it disposes a removed or changed row).
func reloading(ctx *kernel.Context, cfg map[string]any) bool {
	active := map[string]bool{}
	for _, r := range ctx.Rows() {
		if r.Plugin == "orb" && r.State == kernel.StateActive {
			active[r.ID] = true
		}
	}
	for _, r := range ctx.Desired() {
		if active[r.ID] && !r.Disabled && reflect.DeepEqual(r.Config, cfg) {
			return true
		}
	}
	return false
}

// registerOrbCommand takes over /orb from the setup skill for this
// session (orbCommand still forwards everything else to the skill) and
// gives it back on unmount.
func registerOrbCommand(ctx *kernel.Context, reg *commands.Registry, o orbLike, home string) {
	jobs := func() []tools.Running {
		if j, err := kernel.Get[interface{ Running() []tools.Running }](ctx, "job-notices"); err == nil {
			return j.Running()
		}
		return nil
	}
	var prev *commands.CommandInfo
	for _, c := range reg.List() {
		if c.Name == "orb" {
			prev = &c
		}
	}
	reg.Unregister("orb")
	info := commands.CommandInfo{Name: "orb", Usage: "status|logs|stop|restart [fresh]|<slug>", Summary: "this session's orb: status, logs, stop; restart applies the project's current setup; /orb <slug> sets a project up"}
	if err := reg.Register(info, orbCommand(o, home, jobs)); err != nil {
		fmt.Fprintf(os.Stderr, "bough: orb: /orb: %v\n", err)
		return
	}
	ctx.Effect(func() {
		reg.Unregister("orb")
		if prev == nil {
			return
		}
		reg.Register(*prev, func(args string) (string, error) {
			return "", commands.SubmitAction(strings.TrimSpace("/orb " + args))
		})
	})
}

func stopOrb(o *iorb.Orb) {
	sctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	if err := o.Stop(sctx); err != nil {
		fmt.Fprintf(os.Stderr, "bough: orb: stop: %v\n", err)
	}
}

// startTimeout bounds a start: the image build, the container, and
// resume.sh. A restart's build and its swap each get the same.
const startTimeout = 30 * time.Minute

// headlessRun is a person or script running one turn; serve's children
// (origin web) are headless too but stay up to show the failure.
func headlessRun(ctx *kernel.Context) bool {
	origin, _ := kernel.Get[string](ctx, "origin")
	return uiModeOf(ctx) == "headless" && origin != "web"
}

func uiModeOf(ctx *kernel.Context) string {
	m, _ := kernel.Get[string](ctx, "ui-mode")
	return m
}

// say reports an orb problem where the person looks: under the tui a raw
// stderr write paints over the alt screen, so it becomes the "orb-notice"
// the ui shows as a warning row; elsewhere one stderr line per line.
func say(ctx *kernel.Context, uiMode, text string) {
	if uiMode != "tui" {
		fmt.Fprintf(os.Stderr, "bough: %s\n", text)
		return
	}
	prev, _ := kernel.Get[string](ctx, "orb-notice")
	if strings.Contains(prev, text) {
		return
	}
	ctx.Provide("orb-notice", strings.TrimSpace(prev+"\n"+text))
}

// failureReport is an orb failure a person can act on: what failed, the
// log lines that say why (already in err), and the one fix that applies.
func failureReport(slug, phase string, err error) string {
	// "orb: open <session>: orb: image …" says orb and open twice.
	msg := openPrefix.ReplaceAllString(err.Error(), "")
	var fix string
	switch phase {
	case iorb.PhaseBuild:
		fix = fmt.Sprintf("Fix the image recipe: `bough project show %s setup.sh` (or Dockerfile), `bough project write %s setup.sh < fixed.sh`, then `bough project restart` (or /orb restart) rebuilds it in this session. Web: Projects → %s → Orb → Rebuild.", slug, slug, slug)
	case iorb.PhaseSetup:
		fix = fmt.Sprintf("The container runs, but resume.sh failed: `bough project show %s resume.sh`, `bough project write %s resume.sh < fixed.sh`; it reruns on `bough project restart` (or /orb restart). Web: Projects → %s → Orb.", slug, slug, slug)
	default:
		fix = fmt.Sprintf("Check the project's repos and the container runtime: `bough project show %s`. Web: Projects → %s → Orb.", slug, slug)
	}
	return fmt.Sprintf("orb for project %s failed%s: %s\n%s", slug, phaseWord(phase), msg, fix)
}

var openPrefix = regexp.MustCompile(`^(orb: )?((open|restart) \S+: )?(orb: )?`)

func phaseWord(phase string) string {
	switch phase {
	case iorb.PhaseBuild:
		return " to build its image"
	case iorb.PhaseSetup:
		return " setup (resume.sh)"
	}
	return " to start"
}

// startingPromptSection is the model's picture of its container while it
// is still starting: it exists, the tools wait for it, and there is no
// address to give yet.
func startingPromptSection(slug, root string, r projectRole) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Project session: %s. Your shell runs in a Linux container that is still starting (repos syncing, image building or container booting). tools.bash, write and patch wait for it and then run as normal, so use them as you would; do not poll for it or ask the user to wait. Files under %s are shared with the host at the same paths. The container's address is not known yet: once a command has run, the prompt says where servers started here are reachable.\n", slug, root)
	b.WriteString(roleLine(r, slug))
	return b.String()
}

// failedPromptSection tells the model its project container did not start,
// so it explains the error and the fix instead of trying commands that
// cannot run.
func failedPromptSection(slug string, err error) string {
	return fmt.Sprintf("Project session: %s. Its container failed to start, so tools.bash, write and patch are unavailable in this session:\n%v\n"+
		"Tell the user this error plainly. If it comes from the project definition (setup.sh, Dockerfile, resume.sh, project.yml), say what to change and give the exact command, e.g. `bough project show %s setup.sh` and `bough project write %s setup.sh < fixed.sh`; then run `bough project restart` (or the user runs /orb restart), which rebuilds it in this session.", slug, err, slug, slug)
}

// addressSection says where servers in the container are reachable: its
// IP, and the opted-in 127.0.0.1 forwards. Without it the model sent the
// user to localhost, where nothing listens.
func addressSection(b *strings.Builder, st iorb.State) {
	if st.IP != "" {
		fmt.Fprintf(b, "The container's address is %s. A server you start here is not on the user's localhost: the user opens http://%s:<port> from their machine (not localhost), so bind servers to 0.0.0.0, not 127.0.0.1.\n", st.IP, st.IP)
	}
	var fwd []string
	for _, p := range st.Ports {
		if p.Error != "" {
			fmt.Fprintf(b, "Container port %d is not forwarded: %s; use the container address instead.\n", p.Guest, p.Error)
			continue
		}
		fwd = append(fwd, fmt.Sprintf("http://127.0.0.1:%d (container port %d)", p.Host, p.Guest))
	}
	if len(fwd) > 0 {
		fmt.Fprintf(b, "Forwarded to the user's host: %s.\n", strings.Join(fwd, ", "))
	}
	// project.yml ports are fixed when the container is created, so they
	// cannot answer "show me this server" mid-turn. A portal can.
	b.WriteString("To show the user a server running here, call tools.portal.open(<port>) once it is listening: it forwards the port to a 127.0.0.1 URL on their machine and adds it to this session's Portal tab. Portals close when the session ends.\n")
	// The browser is the host's; the guest has no Chrome. See cmd/bough/browser.go.
	b.WriteString("To look at a page yourself, run `bough browser` (agent-browser on the user's machine, which can reach this container's address): `bough browser open <url>`, then `bough browser snapshot -i` for the accessibility tree with refs like @e2, then `bough browser click @e2`. Refs belong to the snapshot that produced them, so take a fresh snapshot after anything that changes the page, and prefer the tree over screenshots.\n")
	if len(st.Ports) == 0 {
		fmt.Fprintf(b, "No ports are forwarded to the host's 127.0.0.1 from project.yml. For a service that should be there on every start, ask the user, then run \"bough project set %s ports 3000,8080:80\" (host:container), then \"bough project restart\" to apply it to this session (the orb is rebuilt and swapped when your turn ends; background jobs stop). For anything ad hoc, use a portal instead.\n", st.Project)
	}
}

// projectRole is the session's place in its project: the main thread,
// one of the threads it started, or neither.
type projectRole struct {
	main   bool   // this session is the project's main thread
	thread bool   // a person started this thread from the project page
	parent string // the session that spawned this one; "" when nothing did
}

// roleLine says which session in the project this is, and what that
// means for the work in front of it. Without it every session in a
// project read the same paragraph, so the main thread had no reason to
// hand anything to a thread and did every job itself.
func roleLine(r projectRole, slug string) string {
	switch {
	case r.main:
		return fmt.Sprintf("You are the MAIN THREAD of %s: one long-lived session, the one the user types to on the project page, and the parent of every other session in the project. Answer questions and do small edits here, in this conversation. For work that runs long or can run on its own, start a thread — tools.spawn(task, {background: true}) — which gets a container of its own in this project and reports back here when it finishes or fails. Say what you handed off; do not poll for it.\n", slug)
	case r.thread:
		// Parented to main only so its report lands there. Told it was a
		// thread that "cannot start threads", the model read that as no
		// delegation at all and never used a subagent.
		return fmt.Sprintf("You are a THREAD of %s, started by the user from the project page. Your last reply reaches the project's main thread when your turn ends. You are a full agent: delegate to subagents and start background agents as any session would; they report back to you.\n", slug)
	case r.parent != "":
		return fmt.Sprintf("You are a THREAD of %s, started as a background agent by session %s (its main thread, or a thread the user started). Finish the task you were given in this container and say what you did: your last reply is what reaches that session. You cannot start threads of your own — if the work needs splitting or a decision, say so in your reply and that session takes it from there.\n", slug, r.parent)
	}
	return ""
}

// promptSection tells the model where its shell runs and what to check.
func promptSection(root, memory string, st iorb.State, def projectdef.Def, missing []string, r projectRole) string {
	checks := def.Checks
	var b strings.Builder
	fmt.Fprintf(&b, "Project session: %s. Your shell runs in a Linux container (%s); files under %s are shared with the host at the same paths.\n", st.Project, st.Container, root)
	b.WriteString(roleLine(r, st.Project))
	if len(def.Identity) > 0 {
		fmt.Fprintf(&b, "Host identity lent to this container: %s (dirs mounted at /root/<dir>, read-only unless :rw; gh = GH_TOKEN).\n", strings.Join(def.Identity, ", "))
	} else {
		b.WriteString("No host identity is lent to this container: no GH_TOKEN and no cloud or cluster config (~/.aws, ~/.kube, ...).\n")
	}
	fmt.Fprintf(&b, "If a command needs one of the user's logins, ask the user to allow it, then run \"bough project add-identity %s gh\" or \"... %s .aws\" (append :rw only for token caches that must refresh); \"bough project restart\" applies it to this session when your turn ends. Network traffic leaves through the host, so internal hosts the user can reach work here too.\n", st.Project, st.Project)
	addressSection(&b, st)
	names := make([]string, 0, len(st.Worktrees))
	for n := range st.Worktrees {
		names = append(names, n)
	}
	sort.Strings(names)
	for _, n := range names {
		fmt.Fprintf(&b, "- repo %s: %s\n", n, st.Worktrees[n])
	}
	if checks.Fast != "" {
		fmt.Fprintf(&b, "Fast check: %s\n", checks.Fast)
	}
	if checks.Full != "" {
		fmt.Fprintf(&b, "Full check: %s\n", checks.Full)
	}
	if st.Status == iorb.StatusFailed && st.Error != "" {
		// The container is up but setup did not finish: without this the
		// agent worked on as if dependencies were installed.
		fmt.Fprintf(&b, "Setup did not finish: %s. Its output is in %s. Read it, tell the user what failed, and fix the definition or ask for the right secret before relying on dependencies.\n", st.Error, filepath.Join(root, "resume.log"))
	}
	if len(missing) > 0 {
		fmt.Fprintf(&b, "Env referenced by resume.sh/checks that may be unset: %s. If so, set them (bough project set %s env.NAME / tools.secret) before trusting checks.\n", strings.Join(missing, ", "), st.Project)
	}
	b.WriteString("If a build or test is blocked by a missing credential, dependency or tool, do not fall back to weaker verification. Find what the repo expects (Makefile, docker-compose, CI config), fix the project definition with `bough project`, ask for secrets with `tools.secret`, and say plainly what stayed unverified.\n")
	fmt.Fprintf(&b, "This project's definition (repos, checks, env, setup.sh, resume.sh) is yours to change with \"bough project ... %s ...\" (no args for usage); it validates, and changes apply to the next session; \"bough project restart\" applies them to this one when your turn ends (the image rebuilds if needed, the container is swapped, background jobs stop, and a notice reports the result).\n", st.Project)
	// One file, edited only when asked. Nothing extracts or summarises
	// into it: a brief the user did not write is a brief they cannot trust.
	fmt.Fprintf(&b, "%s is this project's standing brief, prepended to every session in it. Write it (tools.write) only when the user asks you to remember something for later; keep it short, and never copy a conversation into it.\n", memory)
	return strings.TrimRight(b.String(), "\n")
}
