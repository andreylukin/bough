package serveclient

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// TokenPath is the per-install secret every /api request must carry:
// as the cookie the UI page sets, or as a Bearer header from a client.
func TokenPath(home string) string {
	return filepath.Join(home, ".bough", "serve.token")
}

// ReadToken returns the token, "" when there is none yet.
func ReadToken(home string) string {
	b, err := os.ReadFile(TokenPath(home))
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(b))
}

// LoadToken reads the token, creating it (0600) on first use. O_EXCL,
// so two serves starting at once cannot each write a different one.
func LoadToken(home string) (string, error) {
	if t := ReadToken(home); t != "" {
		return t, nil
	}
	p := TokenPath(home)
	if err := os.MkdirAll(filepath.Dir(p), 0o700); err != nil {
		return "", fmt.Errorf("serveclient: token dir: %w", err)
	}
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("serveclient: token: %w", err)
	}
	tok := hex.EncodeToString(raw)
	f, err := os.OpenFile(p, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if errors.Is(err, fs.ErrExist) {
		if t := ReadToken(home); t != "" {
			return t, nil
		}
		return "", fmt.Errorf("serveclient: token file %s is empty", p)
	}
	if err != nil {
		return "", fmt.Errorf("serveclient: token: %w", err)
	}
	defer f.Close()
	if _, err := f.WriteString(tok + "\n"); err != nil {
		return "", fmt.Errorf("serveclient: token: %w", err)
	}
	return tok, nil
}

// tokens remembers the token found next to each base Addr returned, so
// a Client built from that base authenticates without new plumbing.
var tokens sync.Map // base -> token
