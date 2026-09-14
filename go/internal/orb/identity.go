package orb

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
)

// A project session acts as the user: same cloud, cluster and GitHub
// identity as their own shell. Credentials that live in files are mounted
// read-write (SSO and kube token caches refresh in place, shared with the
// host); the GitHub token lives in the macOS keychain, so it is read on
// the host and passed in.

// identityDirs are $HOME-relative config dirs mounted at /root/<dir>.
var identityDirs = []string{".aws", ".kube", ".config/gcx", ".config/argocd", ".config/gcloud", ".circleci"}

// IdentityEnvPrefixes are host env vars passed through unchanged.
var IdentityEnvPrefixes = []string{"AWS_PROFILE", "AWS_REGION", "AWS_DEFAULT_REGION", "GRAFANA_", "ARGOCD_", "CIRCLECI_", "LINEAR_"}

// identityMounts are the built-in dirs plus the project's own identity
// list (validated by projectdef), each mounted once, when present.
func identityMounts(home string, extra []string) []container.Mount {
	var ms []container.Mount
	seen := map[string]bool{}
	for _, d := range append(append([]string(nil), identityDirs...), extra...) {
		if seen[d] {
			continue
		}
		seen[d] = true
		src := filepath.Join(home, d)
		if fi, err := os.Stat(src); err == nil && fi.IsDir() {
			ms = append(ms, container.Mount{Source: src, Target: filepath.Join("/root", d)})
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
		ghToken.val, ghToken.at = hostCommand("gh", "auth", "token"), time.Now()
	}
	return ghToken.val
}

// identityEnv is the per-exec env that carries the user's identity.
func identityEnv() []string {
	var env []string
	if t := githubToken(); t != "" {
		env = append(env, "GH_TOKEN="+t)
	}
	// Git config through the environment: the host's gitconfig names
	// macOS binaries, so it is not mounted.
	cfg := [][2]string{{"credential.https://github.com.helper", ""}, {"credential.https://github.com.helper", "!gh auth git-credential"}}
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
