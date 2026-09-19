package main

// `bough project list|build|status|logs|stop`: the orb side of a project
// from a shell, on the same orb helpers serve's endpoints use.

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

// loadProject is projectdef.Load with a missing project said plainly.
func loadProject(home, slug string) (projectdef.Project, error) {
	p, err := projectdef.Load(home, slug)
	if errors.Is(err, fs.ErrNotExist) {
		return p, fmt.Errorf("no project %q (bough project list shows them)", slug)
	}
	return p, err
}

// cliOrbStatus is state.json as serve reports it: "running" whose owner
// is gone is stopped.
func cliOrbStatus(s orb.State) orb.State {
	if (s.Status == orb.StatusRunning || s.Status == orb.StatusStarting) && !ownerAlive(s) {
		s.Status = orb.StatusStopped
	}
	return s
}

func projectList(out io.Writer, home string) error {
	ps, err := projectdef.List(home)
	states, _ := orb.List(home)
	rt := projectRuntime()
	tw := tabwriter.NewWriter(out, 2, 8, 2, ' ', 0)
	fmt.Fprintln(tw, "SLUG\tREPOS\tIMAGE\tORBS")
	for _, p := range ps {
		names := make([]string, len(p.Def.Repos))
		for i, r := range p.Def.Repos {
			names[i] = r.RepoName()
		}
		repos := "no repos"
		if len(names) > 0 {
			repos = strings.Join(names, ", ")
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", p.Slug, repos, imageWord(rt, home, p), orbCounts(states, p.Slug))
	}
	tw.Flush()
	return err
}

func imageWord(rt container.Runtime, home string, p projectdef.Project) string {
	b, _ := orb.ReadBuild(home, p.Slug)
	if b.State == "building" {
		return "building"
	}
	h, err := projectdef.ImageHash(home, p)
	if err != nil {
		return "invalid"
	}
	// A failed build can leave its tag behind; the record wins.
	if orb.FailedBuild(home, p.Slug, h) {
		return "build failed"
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	ok, err := rt.ImageExists(ctx, projectdef.ImageTag(p.Slug, h))
	switch {
	case err != nil:
		return "runtime unavailable"
	case ok:
		return "built"
	}
	return "not built"
}

func orbCounts(states []orb.State, slug string) string {
	n := map[orb.Status]int{}
	for _, s := range states {
		if s.Project == slug {
			n[cliOrbStatus(s).Status]++
		}
	}
	var parts []string
	for _, st := range []orb.Status{orb.StatusRunning, orb.StatusStarting, orb.StatusBuilding, orb.StatusStopped, orb.StatusFailed} {
		if n[st] > 0 {
			parts = append(parts, fmt.Sprintf("%d %s", n[st], st))
		}
	}
	if len(parts) == 0 {
		return "-"
	}
	return strings.Join(parts, ", ")
}

func projectBuild(out io.Writer, home string, args []string) error {
	if len(args) != 1 {
		return fmt.Errorf("usage: bough project build <slug>")
	}
	p, err := loadProject(home, args[0])
	if err != nil {
		return err
	}
	ctx := context.Background()
	rt := projectRuntime()
	h, err := projectdef.ImageHash(home, p)
	if err != nil {
		return err
	}
	tag := projectdef.ImageTag(p.Slug, h)
	if ok, err := rt.ImageExists(ctx, tag); err == nil && ok && !orb.FailedBuild(home, p.Slug, h) {
		fmt.Fprintf(out, "Up to date: %s\n", tag)
		return nil
	}
	if err := orb.SyncRepos(ctx, home, p); err != nil {
		return err
	}
	if tag, err = orb.EnsureImage(ctx, rt, home, p, out); err != nil {
		return err
	}
	fmt.Fprintf(out, "Built %s\n", tag)
	return nil
}

// orbOf reads a session's orb or says there is none.
func orbOf(home, session string) (orb.State, error) {
	s, err := orb.ReadState(home, session)
	if err != nil {
		return s, err
	}
	if s.Session == "" {
		return s, fmt.Errorf("no orb for session %q (bough project status lists them)", session)
	}
	return cliOrbStatus(s), nil
}

func phaseText(s orb.State) string {
	line := orb.PhaseLine(s, time.Now())
	_, after, _ := strings.Cut(line, " · ")
	return after
}

func projectStatus(out io.Writer, home string, args []string) error {
	if len(args) > 1 {
		return fmt.Errorf("usage: bough project status [slug|session]")
	}
	if len(args) == 1 {
		if s, err := orb.ReadState(home, args[0]); err == nil && s.Session != "" {
			s = cliOrbStatus(s)
			fmt.Fprintf(out, "session    %s\nproject    %s\nstate      %s\n", s.Session, s.Project, phaseText(s))
			if s.Container != "" {
				fmt.Fprintf(out, "container  %s\n", s.Container)
			}
			if s.Image != "" {
				fmt.Fprintf(out, "image      %s\n", s.Image)
			}
			for name, path := range s.Worktrees {
				fmt.Fprintf(out, "worktree   %s  %s\n", name, path)
			}
			if s.Error != "" {
				fmt.Fprintf(out, "error      %s\n", s.Error)
			}
			_, log := orb.LogFor(home, s)
			fmt.Fprintf(out, "log        %s\nupdated    %s\n", log, s.UpdatedAt.Local().Format("2006-01-02 15:04"))
			return nil
		}
		if _, err := loadProject(home, args[0]); err != nil {
			return fmt.Errorf("no project or orb session %q", args[0])
		}
	}
	var preflightErr error
	if len(args) == 1 {
		preflightErr = projectPreflight(out, home, args[0])
		fmt.Fprintln(out)
	}
	states, err := orb.List(home)
	if err != nil {
		return err
	}
	tw := tabwriter.NewWriter(out, 2, 8, 2, ' ', 0)
	fmt.Fprintln(tw, "SESSION\tPROJECT\tSTATE\tUPDATED")
	for _, s := range states {
		if len(args) == 1 && s.Project != args[0] {
			continue
		}
		s = cliOrbStatus(s)
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", s.Session, s.Project, phaseText(s), s.UpdatedAt.Local().Format("2006-01-02 15:04"))
	}
	if err := tw.Flush(); err != nil {
		return err
	}
	return preflightErr
}

// projectLogs prints a project's build.log, or the log that explains a
// session's orb (resume.log, or build.log when the build failed).
func projectLogs(out io.Writer, home string, args []string) error {
	if len(args) != 1 {
		return fmt.Errorf("usage: bough project logs <slug|session>")
	}
	name, path := "", ""
	if _, err := projectdef.Load(home, args[0]); err == nil {
		name, path = "build.log", orb.ImageLogPath(home, args[0])
	} else {
		s, err := orbOf(home, args[0])
		if err != nil {
			return err
		}
		name, path = orb.LogFor(home, s)
	}
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		fmt.Fprintf(out, "== %s (empty) ==\n", name)
		return nil
	}
	if err != nil {
		return err
	}
	fmt.Fprintf(out, "== %s %s ==\n%s", name, path, b)
	return nil
}

// projectStop stops a session's container; the session's next command
// starts it again. A live owner may have jobs running, so that asks first.
func projectStop(out io.Writer, in io.Reader, home string, args []string) error {
	pos, _, yes, err := cleanupArgs(args)
	if err != nil || len(pos) != 1 {
		return fmt.Errorf("usage: bough project stop <session> [--yes]")
	}
	s, err := orbOf(home, pos[0])
	if err != nil {
		return err
	}
	if ownerAlive(s) {
		fmt.Fprintf(out, "Session %s is running (pid %d); its background jobs stop with the container. The container and worktrees stay.\n", s.Session, s.PID)
		if !confirm(out, in, yes) {
			return nil
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := orb.StopContainer(ctx, projectRuntime(), home, s.Session); err != nil {
		return err
	}
	fmt.Fprintf(out, "Stopped orb %s.\n", s.Session)
	return nil
}
