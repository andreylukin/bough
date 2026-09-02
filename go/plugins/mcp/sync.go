package mcp

// bough sync-mcp: adopt Claude Code's MCP OAuth grants BY REFERENCE.
// The keychain item Claude Code maintains ("Claude Code-credentials")
// holds an mcpOAuth map of <serverName>|<hash> -> {serverName,
// serverUrl, accessToken, expiresAt, ...}. We regenerate exactly one
// file, ~/.bough/mcp.sync.json, with one http server per grant whose
// Authorization header is a ${keychain:…} pointer; no token is ever
// written. A grant that looks expired is kept but disabled with a
// note. ~/.bough/mcp.json (hand-written) outranks this file by name.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const (
	keychainService = "Claude Code-credentials"
	grantsKey       = "mcpOAuth"
)

// grant is one adopted OAuth grant.
type grant struct {
	Key       string // full map key, <serverName>|<hash>
	Name      string // Claude Code's server name
	URL       string
	ExpiresMS int64 // 0 when never recorded
}

func (g grant) stale(now time.Time) bool {
	return g.ExpiresMS == 0 || g.ExpiresMS < now.UnixMilli()
}

// grantsOf parses the credentials payload; grants without a serverUrl
// are reported by name and skipped. Sorted by name.
func grantsOf(payload []byte, warn func(string)) ([]grant, error) {
	var doc map[string]any
	if err := json.Unmarshal(bytesTrim(payload), &doc); err != nil {
		return nil, fmt.Errorf("credentials payload is not JSON: %w", err)
	}
	m, _ := doc[grantsKey].(map[string]any)
	var out []grant
	for key, v := range m {
		e, _ := v.(map[string]any)
		name, _ := e["serverName"].(string)
		if name == "" {
			name, _, _ = strings.Cut(key, "|")
		}
		url, _ := e["serverUrl"].(string)
		if url == "" {
			warn(fmt.Sprintf("grant %q carries no serverUrl; skipped", name))
			continue
		}
		g := grant{Key: key, Name: name, URL: url}
		if f, ok := e["expiresAt"].(float64); ok {
			g.ExpiresMS = int64(f)
		}
		out = append(out, g)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out, nil
}

// rowNames maps grants to server names: the last ":"-segment of Claude
// Code's name ("plugin:slack:slack" -> "slack") unless that collides,
// in which case the full name stands.
func rowNames(grants []grant) []string {
	short := make([]string, len(grants))
	count := map[string]int{}
	for i, g := range grants {
		parts := strings.Split(g.Name, ":")
		short[i] = parts[len(parts)-1]
		count[short[i]]++
	}
	for i, g := range grants {
		if count[short[i]] > 1 {
			short[i] = g.Name
		}
	}
	return short
}

// render builds the mcp.sync.json document.
func render(grants []grant, now time.Time) []byte {
	type entry struct {
		URL      string            `json:"url"`
		Headers  map[string]string `json:"headers"`
		Disabled bool              `json:"disabled,omitempty"`
		Note     string            `json:"note,omitempty"`
	}
	doc := struct {
		Comment string           `json:"_comment"`
		At      time.Time        `json:"at"`
		Servers map[string]entry `json:"servers"`
	}{
		Comment: "Written by `bough sync-mcp` and regenerated wholesale on every run; do not edit. " +
			"Authorization values are keychain pointers resolved at connect time; no token lives here. " +
			"Override a server by name in ~/.bough/mcp.json.",
		At:      now,
		Servers: map[string]entry{},
	}
	names := rowNames(grants)
	for i, g := range grants {
		e := entry{URL: g.URL, Headers: map[string]string{
			"Authorization": fmt.Sprintf("Bearer ${keychain:%s#%s.%s.accessToken}", keychainService, grantsKey, g.Key),
		}}
		if g.stale(now) {
			e.Disabled = true
			e.Note = "grant looks expired: re-auth it in `claude` (/mcp), then run `bough sync-mcp` again"
		}
		doc.Servers[names[i]] = e
	}
	out, _ := json.MarshalIndent(doc, "", "  ")
	return append(out, '\n')
}

func syncPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".bough", "mcp.sync.json"), nil
}

// runSync is the verb: read the grants, regenerate the file (rename
// over the old one), and say what was adopted. --dry-run prints.
func runSync(args []string) error {
	dry := false
	for _, a := range args {
		switch a {
		case "--dry-run", "-n":
			dry = true
		default:
			return fmt.Errorf("usage: bough sync-mcp [--dry-run]")
		}
	}
	payload, err := readSecret(keychainService)
	if err != nil {
		return fmt.Errorf("%w\nRun `claude` and connect an MCP server there first (/mcp), from a GUI session.", err)
	}
	grants, err := grantsOf(payload, func(msg string) { fmt.Fprintln(os.Stderr, "sync-mcp:", msg) })
	if err != nil {
		return err
	}
	now := time.Now()
	out := render(grants, now)
	if dry {
		os.Stdout.Write(out)
		return nil
	}
	path, err := syncPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, out, 0o644); err != nil {
		return err
	}
	if err := os.Rename(tmp, path); err != nil {
		return err
	}
	names := rowNames(grants)
	for i, g := range grants {
		state := "ok"
		if g.stale(now) {
			state = "expired, disabled"
		}
		fmt.Printf("%-20s %-18s %s\n", names[i], state, g.URL)
	}
	fmt.Printf("wrote %s (%d servers); run `bough mcp status` to check them\n", path, len(grants))
	return nil
}
