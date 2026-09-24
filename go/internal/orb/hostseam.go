package orb

import "time"

// StubHost replaces what an orb asks the host outside $HOME: cmd answers
// the host CLIs (`gh auth token`, `git config --global`) and shell stands
// for the login shell's exports. It is for a driver outside this package
// that plays a session's process over container.Fake (tests/model/mbt),
// as secrets.KeychainRead is for the keychain: the real ones read the
// user's gh login, gitconfig and ~/.zshrc. Call it once, before any orb
// in the process runs; it empties both caches.
func StubHost(cmd func(name string, args ...string) string, shell func() map[string]string) {
	hostCommand, hostShellEnv = cmd, shell
	ForgetGitHubToken()
	shellEnvCache.Lock()
	shellEnvCache.val, shellEnvCache.at = nil, time.Time{}
	shellEnvCache.Unlock()
}

// ForgetGitHubToken empties the process's `gh auth token` cache: what a
// new session process starts with, and what five minutes do to it.
func ForgetGitHubToken() {
	ghToken.Lock()
	ghToken.val, ghToken.at = "", time.Time{}
	ghToken.Unlock()
}
