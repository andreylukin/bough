package orb

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"strings"
)

// Every orb's VM shares one bridge, so the host proxy and relay (proxy.go)
// require the orb's own token. It is made when the container is created,
// set in the container's env as BOUGH_ORB_TOKEN and kept in the orb dir
// (0600, never in state.json, which serve serves). A container created
// before tokens has no file: it keeps working unauthenticated, and its
// state says ProxyAuthLegacy until it is removed and recreated.

const (
	tokenFile = "token"
	tokenEnv  = "BOUGH_ORB_TOKEN"

	ProxyAuthToken  = "token"
	ProxyAuthLegacy = "legacy"
)

// LegacyProxyNotice is what the surfaces say about a ProxyAuthLegacy orb.
const LegacyProxyNotice = "This orb predates proxy tokens: any VM on the bridge can use its host proxy and relay. Remove the orb and start a session to recreate it with a token."

// newToken writes a fresh token for a container about to be created.
func newToken(home, session string) (string, error) {
	b := make([]byte, 32)
	rand.Read(b)
	tok := hex.EncodeToString(b)
	path := filepath.Join(Dir(home, session), tokenFile)
	os.Remove(path) // WriteFile keeps an existing file's mode
	if err := os.WriteFile(path, []byte(tok), 0o600); err != nil {
		return "", err
	}
	return tok, nil
}

// readToken is a reused container's token, "" when it predates tokens.
func readToken(home, session string) (string, error) {
	b, err := os.ReadFile(filepath.Join(Dir(home, session), tokenFile))
	if errors.Is(err, os.ErrNotExist) {
		return "", nil
	}
	return strings.TrimSpace(string(b)), err
}

func proxyAuth(token string) string {
	if token == "" {
		return ProxyAuthLegacy
	}
	return ProxyAuthToken
}
