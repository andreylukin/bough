package mcp

// Keychain references: "${keychain:<service>#<json.path>}" names a
// value inside a login-keychain generic password whose payload is
// JSON (Claude Code keeps its MCP OAuth grants that way). Resolved
// only at connect time, via the `security` CLI, so files carry
// pointers and never tokens. Over SSH the login keychain is locked
// and resolution fails loudly; run from a GUI session.

import (
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
)

// readSecret is the keychain reader; a var so tests can stub it.
var readSecret = func(service string) ([]byte, error) {
	out, err := exec.Command("security", "find-generic-password", "-s", service, "-w").Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			msg := strings.TrimSpace(string(ee.Stderr))
			if msg == "" {
				msg = fmt.Sprintf("security exited %d", ee.ExitCode())
			}
			if ee.ExitCode() == 36 || strings.Contains(msg, "not allowed") {
				msg += "; the login keychain is locked to this session (SSH?) — run from a GUI session, or `security unlock-keychain ~/Library/Keychains/login.keychain-db` first"
			}
			return nil, fmt.Errorf("keychain item %q: %s", service, msg)
		}
		return nil, err
	}
	return out, nil
}

// resolveRef expands every ${keychain:service#path} in v.
func resolveRef(v string) (string, error) {
	for {
		i := strings.Index(v, "${keychain:")
		if i < 0 {
			return v, nil
		}
		j := strings.Index(v[i:], "}")
		if j < 0 {
			return "", fmt.Errorf("unterminated keychain reference in %q", v)
		}
		ref := v[i+len("${keychain:") : i+j]
		service, path, ok := strings.Cut(ref, "#")
		if !ok {
			return "", fmt.Errorf("keychain reference %q needs <service>#<path>", ref)
		}
		data, err := readSecret(service)
		if err != nil {
			return "", err
		}
		val, err := jsonPath(data, path)
		if err != nil {
			return "", fmt.Errorf("keychain item %q: %w", service, err)
		}
		v = v[:i] + val + v[i+j+1:]
	}
}

// jsonPath walks a dotted path through a JSON object; at each level
// the longest key prefix of the remaining path wins, so keys that
// themselves contain dots (grant ids) resolve.
func jsonPath(data []byte, path string) (string, error) {
	var cur any
	if err := json.Unmarshal(bytesTrim(data), &cur); err != nil {
		return "", fmt.Errorf("payload is not JSON: %w", err)
	}
	rest := path
	for rest != "" {
		m, ok := cur.(map[string]any)
		if !ok {
			return "", fmt.Errorf("%q: not an object at %q", path, rest)
		}
		best := ""
		for k := range m {
			if (rest == k || strings.HasPrefix(rest, k+".")) && len(k) > len(best) {
				best = k
			}
		}
		if best == "" {
			return "", fmt.Errorf("%q: no key matches %q", path, rest)
		}
		cur = m[best]
		rest = strings.TrimPrefix(strings.TrimPrefix(rest, best), ".")
	}
	switch x := cur.(type) {
	case string:
		return x, nil
	case nil:
		return "", fmt.Errorf("%q: null", path)
	default:
		b, _ := json.Marshal(x)
		return string(b), nil
	}
}

func bytesTrim(b []byte) []byte { return []byte(strings.TrimSpace(string(b))) }
