package orb

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"slices"
	"sort"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
)

// PreflightStatus is one check's outcome.
type PreflightStatus string

const (
	PreflightOK   PreflightStatus = "ok"
	PreflightWarn PreflightStatus = "warn" // works, but not as configured
	PreflightFail PreflightStatus = "fail"
)

// PreflightCheck is one thing a session start needs. Detail never
// carries a secret value.
type PreflightCheck struct {
	Kind   string          `json:"kind"` // runtime, clone, gh, secret
	Name   string          `json:"name"`
	Status PreflightStatus `json:"status"`
	Detail string          `json:"detail,omitempty"`
}

const preflightTimeout = 10 * time.Second

// Preflight answers "would a session start now?" without starting one:
// the runtime is up, every repo can be checked out, the GitHub token is
// there when identity lends gh, and every secret resolves.
func Preflight(ctx context.Context, home string, rt container.Runtime, p projectdef.Project) []PreflightCheck {
	var out []PreflightCheck
	rc := PreflightCheck{Kind: "runtime", Name: rt.Name(), Status: PreflightOK}
	if err := rt.Available(ctx); err != nil {
		rc.Status, rc.Detail = PreflightFail, err.Error()
	}
	out = append(out, rc)
	for _, r := range p.Def.Repos {
		out = append(out, cloneCheck(ctx, home, p.Slug, r))
	}
	if slices.Contains(p.Def.Identity, projectdef.IdentityGitHub) {
		c := PreflightCheck{Kind: "gh", Name: "GitHub token", Status: PreflightOK}
		if githubToken() == "" {
			c.Status, c.Detail = PreflightFail, "`gh auth token` printed nothing (run: gh auth login)"
		}
		out = append(out, c)
	}
	names := make([]string, 0, len(p.Def.Secrets))
	for n := range p.Def.Secrets {
		names = append(names, n)
	}
	sort.Strings(names)
	for _, n := range names {
		c := PreflightCheck{Kind: "secret", Name: n, Status: PreflightOK}
		v, err := secrets.Resolve(p.Def.Secrets[n])
		switch {
		case errors.Is(err, secrets.ErrNotFound):
			c.Status, c.Detail = PreflightFail, p.Def.Secrets[n]+" not found in the keychain"
		case err != nil:
			c.Status, c.Detail = PreflightFail, err.Error()
		case v == "":
			c.Status, c.Detail = PreflightFail, p.Def.Secrets[n]+" is empty"
		}
		out = append(out, c)
	}
	return out
}

// cloneCheck: a path repo must be a git checkout; a remote must answer
// ls-remote, or at least have a cached clone to fall back on.
func cloneCheck(ctx context.Context, home, slug string, r projectdef.Repo) PreflightCheck {
	c := PreflightCheck{Kind: "clone", Name: r.RepoName(), Status: PreflightOK}
	ctx, cancel := context.WithTimeout(ctx, preflightTimeout)
	defer cancel()
	if r.Path != "" {
		dir := projectdef.ExpandPath(home, r.Path)
		if _, err := gitOut(ctx, dir, "rev-parse", "--git-dir"); err != nil {
			c.Status, c.Detail = PreflightFail, r.Path+" is not a git checkout"
		}
		return c
	}
	cmd := exec.CommandContext(ctx, "git", "ls-remote", "--exit-code", "--quiet", r.Remote, "HEAD")
	cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0", "GIT_SSH_COMMAND=ssh -o BatchMode=yes")
	out, err := cmd.CombinedOutput()
	if err == nil {
		return c
	}
	why := lastLine(string(out))
	if why == "" {
		why = err.Error()
	}
	if _, serr := os.Stat(projectdef.CacheGitDir(home, slug, r)); serr == nil {
		c.Status, c.Detail = PreflightWarn, "remote unreachable, the cached clone is used: "+why
		return c
	}
	c.Status, c.Detail = PreflightFail, "cannot reach "+r.Remote+": "+why
	return c
}

func lastLine(s string) string {
	lines := strings.Split(strings.TrimSpace(s), "\n")
	return strings.TrimSpace(lines[len(lines)-1])
}
