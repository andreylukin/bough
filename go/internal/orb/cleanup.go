package orb

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

// RemovePlan is what removing one session's orb deletes and keeps, worked
// out before anything is touched so a confirm can show it exactly.
type RemovePlan struct {
	Session   string       `json:"session"`
	Project   string       `json:"project"`
	Status    Status       `json:"status"`
	Container string       `json:"container,omitempty"` // "" when there is none
	Dir       string       `json:"dir"`
	Worktrees []string     `json:"worktrees"`
	Bytes     int64        `json:"bytes"` // disk the orb dir (worktrees included) frees
	Branches  []BranchPlan `json:"branches"`
}

// BranchPlan is one bough/<session> branch. Delete is set only when the
// caller asked for branches and its commits are safe elsewhere.
type BranchPlan struct {
	Repo   string `json:"repo"`
	GitDir string `json:"gitDir"`
	Branch string `json:"branch"`
	Delete bool   `json:"delete"`
	Reason string `json:"reason"`
}

// List is every orb with a state.json, newest first.
func List(home string) ([]State, error) {
	ents, err := os.ReadDir(orbsRoot(home))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []State
	for _, e := range ents {
		if !e.IsDir() || e.Name() == "cache" || e.Name() == "images" {
			continue
		}
		if s, err := ReadState(home, e.Name()); err == nil && s.Session != "" {
			out = append(out, s)
		}
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].UpdatedAt.After(out[j].UpdatedAt) })
	return out, nil
}

// PlanRemove works out what Remove would do to session's orb. With
// branches, each bough/<session> branch fully merged into its repo's base
// or contained in a remote-tracking ref is marked for deletion; the rest
// are kept with the reason.
func PlanRemove(ctx context.Context, rt container.Runtime, home, session string, branches bool) (RemovePlan, error) {
	if err := checkSession(session); err != nil {
		return RemovePlan{}, err
	}
	st, err := ReadState(home, session)
	if err != nil {
		return RemovePlan{}, err
	}
	dir := Dir(home, session)
	if _, err := os.Stat(dir); err != nil {
		return RemovePlan{}, fmt.Errorf("orb: remove: no orb for session %q", session)
	}
	plan := RemovePlan{Session: session, Project: st.Project, Status: st.Status, Dir: dir, Worktrees: []string{}, Branches: []BranchPlan{}}
	name := container.OrbName(session)
	if cs, err := rt.Inspect(ctx, name); err != nil || cs != container.StateMissing {
		plan.Container = name // an engine that cannot answer still gets a delete
	}
	ents, _ := os.ReadDir(dir)
	for _, e := range ents {
		if fi, err := os.Stat(filepath.Join(dir, e.Name(), ".git")); err == nil && !fi.IsDir() {
			plan.Worktrees = append(plan.Worktrees, filepath.Join(dir, e.Name()))
		}
	}
	filepath.WalkDir(dir, func(_ string, d fs.DirEntry, err error) error {
		if err == nil && d.Type().IsRegular() {
			if fi, err := d.Info(); err == nil {
				plan.Bytes += fi.Size()
			}
		}
		return nil
	})
	branch := "bough/" + session
	seen := map[string]bool{}
	add := func(repo, gd, base string) {
		if seen[repo] {
			return
		}
		seen[repo] = true
		if _, err := gitOut(ctx, gd, "rev-parse", "--verify", "--quiet", "refs/heads/"+branch); err != nil {
			return
		}
		b := BranchPlan{Repo: repo, GitDir: gd, Branch: branch}
		safe := ""
		if _, err := gitOut(ctx, gd, "merge-base", "--is-ancestor", branch, base); err == nil {
			safe = "merged into " + base
		} else if out, _ := gitOut(ctx, gd, "for-each-ref", "--contains", branch, "--format=%(refname:short)", "refs/remotes"); out != "" {
			safe = "pushed to " + strings.Fields(out)[0]
		}
		switch {
		case safe == "":
			b.Reason = "has commits not merged or pushed"
		case branches:
			b.Delete, b.Reason = true, safe
		default:
			b.Reason = safe + "; kept unless branches are pruned"
		}
		plan.Branches = append(plan.Branches, b)
	}
	if p, err := projectdef.Load(home, st.Project); err == nil {
		for _, r := range p.Def.Repos {
			add(r.RepoName(), projectdef.SourceGitDir(home, st.Project, r), r.BaseRef())
		}
	}
	for repo, wt := range st.Worktrees {
		if gd, err := commonGitDir(ctx, wt); err == nil {
			add(repo, gd, "HEAD")
		}
	}
	return plan, nil
}

// RemovePlanned removes the orb and then deletes the branches the plan
// marked; branches go last because git refuses one a worktree holds.
func RemovePlanned(ctx context.Context, rt container.Runtime, home string, plan RemovePlan) error {
	errs := []error{Remove(ctx, rt, home, plan.Session)}
	for _, b := range plan.Branches {
		if b.Delete {
			if _, err := gitOut(ctx, b.GitDir, "branch", "-D", b.Branch); err != nil {
				errs = append(errs, err)
			}
		}
	}
	return errors.Join(errs...)
}

// PruneImages removes slug's image tags other than keep that no container
// uses, returning the tags it removed.
func PruneImages(ctx context.Context, rt container.Runtime, slug, keep string) ([]string, error) {
	before, err := rt.Images(ctx)
	if err != nil {
		return nil, err
	}
	perr := pruneImages(ctx, rt, slug, keep)
	after, _ := rt.Images(ctx)
	var gone []string
	for _, t := range before {
		if strings.HasPrefix(t, "bough-orb/"+slug+":") && !slices.Contains(after, t) {
			gone = append(gone, t)
		}
	}
	return gone, perr
}

func checkSession(session string) error {
	if session == "" || strings.ContainsAny(session, `/\`) || session == "cache" || session == "images" || session == "." || session == ".." {
		return fmt.Errorf("orb: remove: bad session id %q", session)
	}
	return nil
}
