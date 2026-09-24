package orb

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

// A project session gets none of the user's identity unless its
// definition lists it (projectdef.Def.Identity): "<dir>" mounts
// $HOME/<dir> read-only at /root/<dir>, "<dir>:rw" read-write (SSO and
// kube token caches that refresh in place), and "gh" passes the host's
// GitHub token, which lives in the macOS keychain, as GH_TOKEN.

// IdentityEnvPrefixes are host env vars passed through unchanged. They
// select a profile or region, never carry a credential.
var IdentityEnvPrefixes = []string{"AWS_PROFILE", "AWS_REGION", "AWS_DEFAULT_REGION"}

// identityMounts are the project's identity dirs (validated by
// projectdef), each mounted once, when present on the host.
func identityMounts(home string, identity []string) []container.Mount {
	var ms []container.Mount
	seen := map[string]bool{}
	for _, entry := range identity {
		d, rw := projectdef.IdentityDir(entry)
		if d == "" || seen[d] {
			continue
		}
		seen[d] = true
		src := filepath.Join(home, d)
		if fi, err := os.Stat(src); err == nil && fi.IsDir() {
			ms = append(ms, container.Mount{Source: src, Target: filepath.Join("/root", d), ReadOnly: !rw})
		}
	}
	return ms
}

// hostCommand runs a host CLI with a short timeout; children may have a
// minimal PATH, so Homebrew's bin is tried first.
var hostCommand = func(name string, args ...string) string {
	bin := name
	if p := filepath.Join("/opt/homebrew/bin", name); fileExists(p) {
		bin = p
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, bin, args...).Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

func fileExists(p string) bool { _, err := os.Stat(p); return err == nil }

// HostGitHubToken is the host's `gh auth token`. A seam: a test that
// runs serve in process swaps it, and never reads the host's gh login
// (hostCommand prefers /opt/homebrew/bin/gh, so PATH cannot stand in).
var HostGitHubToken = func() string { return hostCommand("gh", "auth", "token") }

var ghToken = struct {
	sync.Mutex
	val string
	at  time.Time
}{}

// githubToken is the host's `gh auth token`, cached for five minutes so
// a busy session does not hit the keychain on every command.
func githubToken() string {
	ghToken.Lock()
	defer ghToken.Unlock()
	if time.Since(ghToken.at) > 5*time.Minute || ghToken.val == "" {
		ghToken.val, ghToken.at = HostGitHubToken(), time.Now()
	}
	return ghToken.val
}

// githubTokenNow reads `gh auth token` now and keeps it for
// githubToken. Preflight answers "would a start work now?": a
// five-minute-old token said ok for a gh the person had just logged out.
func githubTokenNow() string {
	ghToken.Lock()
	defer ghToken.Unlock()
	ghToken.val, ghToken.at = HostGitHubToken(), time.Now()
	return ghToken.val
}

// identityEnv is the per-exec env that carries the user's identity: git
// author and profile env always, GH_TOKEN only when identity lists "gh".
func identityEnv(identity []string) []string {
	var env []string
	// Git config through the environment: the host's gitconfig names
	// macOS binaries, so it is not mounted.
	var cfg [][2]string
	if slices.Contains(identity, projectdef.IdentityGitHub) {
		if t := githubToken(); t != "" {
			env = append(env, "GH_TOKEN="+t)
		}
		cfg = append(cfg, [2]string{"credential.https://github.com.helper", ""}, [2]string{"credential.https://github.com.helper", "!gh auth git-credential"})
	}
	for _, k := range []string{"user.name", "user.email"} {
		if v := hostCommand("git", "config", "--global", k); v != "" {
			cfg = append(cfg, [2]string{k, v})
		}
	}
	env = append(env, "GIT_CONFIG_COUNT="+strconv.Itoa(len(cfg)))
	for i, kv := range cfg {
		env = append(env, "GIT_CONFIG_KEY_"+strconv.Itoa(i)+"="+kv[0], "GIT_CONFIG_VALUE_"+strconv.Itoa(i)+"="+kv[1])
	}
	for _, e := range os.Environ() {
		k, _, _ := strings.Cut(e, "=")
		for _, p := range IdentityEnvPrefixes {
			if k == p || (strings.HasSuffix(p, "_") && strings.HasPrefix(k, p)) {
				env = append(env, e)
				break
			}
		}
	}
	return env
}
