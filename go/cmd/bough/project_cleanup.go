package main

// `bough project rm|prune`: remove orbs only on this explicit action.
// Each prints exactly what it deletes and keeps, then asks unless --yes.

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

var projectRuntime = container.Default

// cleanupArgs splits positional args from --branches and --yes.
func cleanupArgs(args []string) (pos []string, branches, yes bool, err error) {
	for _, a := range args {
		switch a {
		case "--branches", "-branches":
			branches = true
		case "--yes", "-yes", "-y":
			yes = true
		default:
			if strings.HasPrefix(a, "-") {
				return nil, false, false, fmt.Errorf("unknown flag %s", a)
			}
			pos = append(pos, a)
		}
	}
	return pos, branches, yes, nil
}

func ownerAlive(s orb.State) bool {
	if s.PID <= 0 {
		return false
	}
	p, err := os.FindProcess(s.PID)
	return err == nil && p.Signal(syscall.Signal(0)) == nil
}

// archivedSessions reads serve's meta.json; a missing file archives nothing.
func archivedSessions(home string) map[string]bool {
	var m struct {
		Sessions map[string]struct {
			Archived bool `json:"archived"`
		} `json:"sessions"`
	}
	b, _ := os.ReadFile(filepath.Join(home, ".bough", "serve", "meta.json"))
	json.Unmarshal(b, &m)
	out := map[string]bool{}
	for id, s := range m.Sessions {
		out[id] = s.Archived
	}
	return out
}

func humanBytes(n int64) string {
	const unit = 1024
	if n < unit {
		return fmt.Sprintf("%d B", n)
	}
	div, exp := int64(unit), 0
	for m := n / unit; m >= unit; m /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %cB", float64(n)/float64(div), "KMGTPE"[exp])
}

func printPlan(out io.Writer, p orb.RemovePlan) {
	fmt.Fprintf(out, "delete  orb %s (project %s, %s), %s on disk\n", p.Session, p.Project, statusWord(p.Status), humanBytes(p.Bytes))
	if p.Container != "" {
		fmt.Fprintf(out, "          container %s\n", p.Container)
	}
	for _, wt := range p.Worktrees {
		fmt.Fprintf(out, "          worktree %s\n", wt)
	}
	fmt.Fprintf(out, "          %s\n", p.Dir)
	for _, wt := range p.Dirty {
		fmt.Fprintf(out, "  refuse  worktree %s has uncommitted changes; commit or discard them first\n", wt)
	}
	for _, b := range p.Branches {
		verb := "keep  "
		if b.Delete {
			verb = "delete"
		}
		fmt.Fprintf(out, "  %s  branch %s in %s: %s\n", verb, b.Branch, b.Repo, b.Reason)
	}
}

func statusWord(s orb.Status) string {
	if s == "" {
		return "no status"
	}
	return string(s)
}

func confirm(out io.Writer, in io.Reader, yes bool) bool {
	if yes {
		return true
	}
	fmt.Fprint(out, "Proceed? [y/N] ")
	line, _ := bufio.NewReader(in).ReadString('\n')
	ok := strings.EqualFold(strings.TrimSpace(line), "y") || strings.EqualFold(strings.TrimSpace(line), "yes")
	if !ok {
		fmt.Fprintln(out, "Nothing removed.")
	}
	return ok
}

func projectRm(out io.Writer, in io.Reader, home string, args []string) error {
	pos, branches, yes, err := cleanupArgs(args)
	if err != nil || len(pos) != 1 {
		return fmt.Errorf("usage: bough project rm <session> [--branches] [--yes]")
	}
	ctx := context.Background()
	st, err := orb.ReadState(home, pos[0])
	if err != nil {
		return err
	}
	if st.Session == "" {
		return fmt.Errorf("no orb for session %q", pos[0])
	}
	if ownerAlive(st) {
		return fmt.Errorf("session %s is still running (pid %d); quit or archive it first", st.Session, st.PID)
	}
	rt := projectRuntime()
	plan, err := orb.PlanRemove(ctx, rt, home, st.Session, branches)
	if err != nil {
		return err
	}
	printPlan(out, plan)
	if !confirm(out, in, yes) {
		return nil
	}
	if err := orb.RemovePlanned(ctx, rt, home, plan); err != nil {
		return err
	}
	fmt.Fprintf(out, "Removed orb %s, freed %s.\n", plan.Session, humanBytes(plan.Bytes))
	return nil
}

// projectPrune removes the orbs nobody resumes: failed ones (a start that
// never finished included) and archived sessions', whose owner is gone.
// Stopped orbs of live sessions stay resumable.
func projectPrune(out io.Writer, in io.Reader, home string, args []string) error {
	pos, branches, yes, err := cleanupArgs(args)
	if err != nil || len(pos) > 1 {
		return fmt.Errorf("usage: bough project prune [slug] [--branches] [--yes]")
	}
	slug := ""
	if len(pos) == 1 {
		slug = pos[0]
	}
	ctx := context.Background()
	states, err := orb.List(home)
	if err != nil {
		return err
	}
	archived := archivedSessions(home)
	rt := projectRuntime()
	var plans []orb.RemovePlan
	var freed int64
	slugs := map[string]bool{}
	for _, s := range states {
		if slug != "" && s.Project != slug {
			continue
		}
		slugs[s.Project] = true
		alive := ownerAlive(s)
		dead := s.Status == orb.StatusFailed || s.Status == orb.StatusStarting || s.Status == orb.StatusBuilding || archived[s.Session]
		if alive || !dead {
			why := "resumable"
			if alive {
				why = "session running"
			}
			fmt.Fprintf(out, "keep    orb %s (project %s, %s): %s\n", s.Session, s.Project, statusWord(s.Status), why)
			continue
		}
		plan, err := orb.PlanRemove(ctx, rt, home, s.Session, branches)
		if err != nil {
			return err
		}
		printPlan(out, plan)
		plans = append(plans, plan)
		freed += plan.Bytes
	}
	if slug != "" {
		slugs[slug] = true
	}
	for s := range slugs {
		fmt.Fprintf(out, "delete  images of %s that no container uses, except the current build\n", s)
	}
	if len(plans) == 0 && len(slugs) == 0 {
		fmt.Fprintln(out, "No orbs to prune.")
		return nil
	}
	if !confirm(out, in, yes) {
		return nil
	}
	var errs []string
	for _, p := range plans {
		if err := orb.RemovePlanned(ctx, rt, home, p); err != nil {
			errs = append(errs, err.Error())
		}
	}
	for s := range slugs {
		keep := ""
		if p, err := projectdef.Load(home, s); err == nil {
			if h, err := projectdef.ImageHash(home, p); err == nil {
				keep = projectdef.ImageTag(s, h)
			}
		}
		if keep == "" {
			continue // without the current tag, every tag might be it
		}
		gone, err := orb.PruneImages(ctx, rt, s, keep)
		for _, t := range gone {
			fmt.Fprintf(out, "Removed image %s\n", t)
		}
		if err != nil {
			errs = append(errs, err.Error())
		}
	}
	fmt.Fprintf(out, "Removed %d orbs, freed %s.\n", len(plans), humanBytes(freed))
	if len(errs) > 0 {
		return fmt.Errorf("%s", strings.Join(errs, "; "))
	}
	return nil
}
