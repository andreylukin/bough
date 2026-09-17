package orb

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/container"
)

// B1: the plan names what Remove deletes and what it keeps, branches are
// kept unless asked, and --branches deletes only merged or pushed ones.
func TestPlanRemoveBranches(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	merged, unpushed := newRepo(t), newRepo(t)
	p := newProject(t, home, "cl", "  - path: "+merged+"\n    name: merged\n    branch: main\n  - path: "+unpushed+"\n    name: unpushed\n    branch: main\n")
	rt := container.NewFake()
	o, err := Open(ctx, rt, home, "s1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	wt := o.State().Worktrees["unpushed"]
	os.WriteFile(filepath.Join(wt, "work.txt"), []byte(strings.Repeat("x", 5000)), 0o644)
	git(t, wt, "add", "-A")
	git(t, wt, "commit", "-qm", "work")

	keep, err := PlanRemove(ctx, rt, home, "s1", false)
	if err != nil {
		t.Fatal(err)
	}
	if keep.Container != container.OrbName("s1") || len(keep.Worktrees) != 2 || keep.Bytes < 5000 {
		t.Fatalf("plan %+v", keep)
	}
	for _, b := range keep.Branches {
		if b.Delete {
			t.Fatalf("branch deleted without --branches: %+v", b)
		}
	}

	plan, err := PlanRemove(ctx, rt, home, "s1", true)
	if err != nil {
		t.Fatal(err)
	}
	got := map[string]BranchPlan{}
	for _, b := range plan.Branches {
		got[b.Repo] = b
	}
	if !got["merged"].Delete || got["unpushed"].Delete || got["unpushed"].Reason == "" {
		t.Fatalf("branches %+v", plan.Branches)
	}

	if err := RemovePlanned(ctx, rt, home, plan); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(Dir(home, "s1")); !os.IsNotExist(err) {
		t.Fatal("orb dir kept")
	}
	if out := git(t, merged, "branch", "--list", "bough/s1"); out != "" {
		t.Fatalf("merged branch kept: %q", out)
	}
	if out := git(t, unpushed, "branch", "--list", "bough/s1"); out == "" {
		t.Fatal("unpushed branch deleted")
	}
}

// A pushed branch is safe to delete even when unmerged.
func TestPlanRemovePushedBranch(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	src := newRepo(t)
	remote := t.TempDir()
	git(t, remote, "init", "-q", "--bare")
	git(t, src, "remote", "add", "origin", remote)
	p := newProject(t, home, "pu", "  - path: "+src+"\n    branch: main\n")
	rt := container.NewFake()
	o, err := Open(ctx, rt, home, "s2", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	wt := o.State().Primary
	os.WriteFile(filepath.Join(wt, "w"), []byte("w"), 0o644)
	git(t, wt, "add", "-A")
	git(t, wt, "commit", "-qm", "w")
	git(t, wt, "push", "-q", "origin", "bough/s2")
	plan, err := PlanRemove(ctx, rt, home, "s2", true)
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.Branches) != 1 || !plan.Branches[0].Delete {
		t.Fatalf("pushed branch %+v", plan.Branches)
	}
}

// A session whose start failed before worktrees still plans and removes.
func TestPlanRemoveFailedStart(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	if err := writeState(home, State{Session: "s3", Project: "gone", Status: StatusFailed, Error: "build failed"}); err != nil {
		t.Fatal(err)
	}
	rt := container.NewFake()
	plan, err := PlanRemove(ctx, rt, home, "s3", true)
	if err != nil {
		t.Fatal(err)
	}
	if plan.Container != "" || len(plan.Worktrees) != 0 || plan.Status != StatusFailed {
		t.Fatalf("plan %+v", plan)
	}
	if err := RemovePlanned(ctx, rt, home, plan); err != nil {
		t.Fatal(err)
	}
	if list, _ := List(home); len(list) != 0 {
		t.Fatalf("still listed %+v", list)
	}
}
