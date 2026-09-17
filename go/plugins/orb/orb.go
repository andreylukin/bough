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
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
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
	o    *iorb.Orb
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
		return container.Podman{}, nil
	case "fake":
		return container.NewFake(), nil
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
	h, err := kernel.Get[pather](ctx, "history")
	if err != nil {
		return fmt.Errorf("orb: %w", err)
	}
	pad, err := kernel.Get[direr](ctx, "scratch")
	if err != nil {
		return fmt.Errorf("orb: %w", err)
	}
	session := strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
	home, err := userHome()
	if err != nil {
		return fmt.Errorf("orb: home dir: %w", err)
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		return fmt.Errorf("orb: open %s: %w", slug, err)
	}
	// Open can build an image; bound it so a wedged engine fails the
	// row instead of hanging the mount forever.
	octx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
	defer cancel()
	if err := rt.Available(octx); err != nil {
		return fmt.Errorf("orb: open %s: runtime %s: %w (run `bough update` or `container system start`)", slug, rt.Name(), err)
	}
	opened.Lock()
	prev, reused := opened.m[session]
	delete(opened.m, session)
	opened.Unlock()
	reused = reused && prev.slug == slug && reflect.DeepEqual(prev.cfg, cfg)
	if !reused && prev.o != nil {
		stopOrb(prev.o)
	}
	o := prev.o
	if !reused {
		// Before the TUI is up the terminal would stay blank for a whole
		// image build: one line follows the start's phases instead.
		stopProgress := func() {}
		if mode, _ := kernel.Get[string](ctx, "ui-mode"); mode == "tui" && !uiActive(ctx) && isTerminal(os.Stderr) {
			stopProgress = progress(os.Stderr, home, session, 250*time.Millisecond)
		}
		o, err = iorb.Open(octx, rt, home, session, p, pad.Dir())
		stopProgress()
		if err != nil {
			// Not a row failure: the first mount is strict, so an error here
			// killed the whole session process before it read stdin, and a
			// message sent to it vanished with no error anywhere. A broken
			// setup.sh made the session unreachable. The session stays up
			// without an orb (tools.bash refuses: "project orb not ready"),
			// says why, and the next start retries the build.
			fmt.Fprintf(os.Stderr, "bough: orb: %s: %v\n", slug, err)
			if s, serr := kernel.Get[sections](ctx, "prompt-sections"); serr == nil {
				s.Set("orb", failedPromptSection(slug, err))
				ctx.Effect(func() { s.Set("orb", "") })
			}
			return nil
		}
	}
	st := o.State()
	if st.ProxyAuth == iorb.ProxyAuthLegacy && !reused {
		fmt.Fprintf(os.Stderr, "bough: orb: %s: %s\n", slug, iorb.LegacyProxyNotice)
	}
	// Checkpoints and relative paths use the process cwd: it must be the
	// primary worktree, which is also the container's workdir.
	if st.Primary != "" {
		if err := chdir(st.Primary); err != nil {
			// Open started the container and no Effect owns it yet.
			stopOrb(o)
			return fmt.Errorf("orb: chdir %s: %w", st.Primary, err)
		}
	}
	resume, _ := projectdef.ReadFile(home, slug, projectdef.FileResume)
	setup, _ := projectdef.ReadFile(home, slug, projectdef.FileSetup)
	dockerfile, _ := projectdef.ReadFile(home, slug, projectdef.FileDockerfile)
	missing := missingEnv(resume, p.Def.Checks, p.Def, setup+"\n"+dockerfile)
	if len(missing) > 0 && !reused {
		fmt.Fprintf(os.Stderr, "bough: orb: %s: unset env %s (resume.sh/checks)\n", slug, strings.Join(missing, ", "))
	}
	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		s.Set("orb", promptSection(o.Root(), st, p.Def, missing))
		ctx.Effect(func() { s.Set("orb", "") })
	}
	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		registerOrbCommand(ctx, reg, o, home)
	}
	ctx.Provide("orb", o)
	ctx.Provide("orb-state", o)
	// Stop, never Remove: a resumed session reuses its worktrees and
	// container. A reload (this row still mounted and still desired as
	// is) keeps the orb running for the next Apply instead.
	ctx.Effect(func() {
		if reloading(ctx, cfg) {
			opened.Lock()
			opened.m[session] = openOrb{o: o, slug: slug, cfg: cfg}
			opened.Unlock()
			return
		}
		stopOrb(o)
	})
	return nil
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
func registerOrbCommand(ctx *kernel.Context, reg *commands.Registry, o *iorb.Orb, home string) {
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
	info := commands.CommandInfo{Name: "orb", Usage: "status|logs|stop|<slug>", Summary: "this session's orb: status, logs, stop; /orb <slug> sets a project up"}
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

// uiActive reports whether the ui row is already mounted (the TUI owns
// the terminal, so nothing may print to it).
func uiActive(ctx *kernel.Context) bool {
	for _, r := range ctx.Rows() {
		if r.Plugin == "ui" && r.State == kernel.StateActive {
			return true
		}
	}
	return false
}

func isTerminal(f *os.File) bool {
	fi, err := f.Stat()
	return err == nil && fi.Mode()&os.ModeCharDevice != 0
}

func stopOrb(o *iorb.Orb) {
	sctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	if err := o.Stop(sctx); err != nil {
		fmt.Fprintf(os.Stderr, "bough: orb: stop: %v\n", err)
	}
}

// failedPromptSection tells the model its project container did not start,
// so it explains the error and the fix instead of trying commands that
// cannot run.
func failedPromptSection(slug string, err error) string {
	return fmt.Sprintf("Project session: %s. Its container failed to start, so tools.bash, write and patch are unavailable in this session:\n%v\n"+
		"Tell the user this error plainly. If it comes from the project definition (setup.sh, Dockerfile, resume.sh, project.yml), say what to change and give the exact command, e.g. `bough project show %s setup.sh` and `bough project write %s setup.sh < fixed.sh`; the next session start rebuilds.", slug, err, slug, slug)
}

// promptSection tells the model where its shell runs and what to check.
func promptSection(root string, st iorb.State, def projectdef.Def, missing []string) string {
	checks := def.Checks
	var b strings.Builder
	fmt.Fprintf(&b, "Project session: %s. Your shell runs in a Linux container (%s); files under %s are shared with the host at the same paths.\n", st.Project, st.Container, root)
	if len(def.Identity) > 0 {
		fmt.Fprintf(&b, "Host identity lent to this container: %s (dirs mounted at /root/<dir>, read-only unless :rw; gh = GH_TOKEN).\n", strings.Join(def.Identity, ", "))
	} else {
		b.WriteString("No host identity is lent to this container: no GH_TOKEN and no cloud or cluster config (~/.aws, ~/.kube, ...).\n")
	}
	fmt.Fprintf(&b, "If a command needs one of the user's logins, ask the user to allow it, then run \"bough project add-identity %s gh\" or \"... %s .aws\" (append :rw only for token caches that must refresh); it applies to the next session. Network traffic leaves through the host, so internal hosts the user can reach work here too.\n", st.Project, st.Project)
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
	fmt.Fprintf(&b, "This project's definition (repos, checks, env, setup.sh, resume.sh) is yours to change with \"bough project ... %s ...\" (no args for usage); it validates, and changes apply to the next session.\n", st.Project)
	return strings.TrimRight(b.String(), "\n")
}
