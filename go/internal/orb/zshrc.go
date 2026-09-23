package orb

import (
	"bytes"
	"context"
	"os/exec"
	"os/user"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
)

// The user's ~/.zshrc exports (API keys, tokens) reach every project exec
// the way secrets do: in ExecOptions.Secrets, never argv or the run env,
// and redacted in output. Only what the rc adds over a bare zsh is taken,
// minus host paths, reserved names and what the project sets itself.

// shellEnvSkip are rc exports that only mean something on the host:
// pager/ls styling, and uv's index that authenticates through the host
// keyring.
var shellEnvSkip = []string{"LESS", "LSCOLORS", "LS_COLORS", "PAGER", "UV_INDEX_URL", "UV_KEYRING_PROVIDER"}

var shellEnvCache = struct {
	sync.Mutex
	val map[string]string
	at  time.Time
}{}

// cachedShellEnv is hostShellEnv, cached for five minutes: loading the rc
// takes seconds and some of it (CodeArtifact) refreshes hourly.
func cachedShellEnv() map[string]string {
	shellEnvCache.Lock()
	defer shellEnvCache.Unlock()
	if time.Since(shellEnvCache.at) > 5*time.Minute {
		shellEnvCache.val, shellEnvCache.at = hostShellEnv(), time.Now()
	}
	return shellEnvCache.val
}

// hostShellEnv is what the host's interactive zsh exports beyond a bare
// one; nil when there is no ~/.zshrc or zsh fails.
var hostShellEnv = func() map[string]string {
	u, err := user.Current()
	if err != nil || !fileExists(filepath.Join(u.HomeDir, ".zshrc")) {
		return nil
	}
	rc := zshEnv(u, "-ic")
	if rc == nil {
		return nil
	}
	return shellEnvDiff(rc, zshEnv(u, "-fc"), u.HomeDir)
}

// zshEnv runs `env -0` in zsh from a clean environment, so the result
// does not depend on how bough itself was started.
func zshEnv(u *user.User, flag string) map[string]string {
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "/bin/zsh", flag, "env -0")
	cmd.Env = []string{"HOME=" + u.HomeDir, "USER=" + u.Username, "LOGNAME=" + u.Username,
		"SHELL=/bin/zsh", "TERM=dumb", "PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"}
	out, err := cmd.Output()
	if err != nil {
		return nil
	}
	env := map[string]string{}
	for _, kv := range bytes.Split(out, []byte{0}) {
		if k, v, ok := strings.Cut(string(kv), "="); ok && k != "" {
			env[k] = v
		}
	}
	return env
}

// shellEnvDiff is rc minus base: names the rc added or changed, without
// reserved names, host-only exports or values naming host paths.
func shellEnvDiff(rc, base map[string]string, home string) map[string]string {
	out := map[string]string{}
	for k, v := range rc {
		if b, ok := base[k]; ok && b == v {
			continue
		}
		if projectdef.ReservedEnvName(k) || slices.Contains(shellEnvSkip, k) ||
			strings.Contains(v, home) || strings.Contains(v, "/opt/homebrew") {
			continue
		}
		out[k] = v
	}
	return out
}
