package projectdef

import (
	"strings"
	"testing"
)

func TestParseIdentity(t *testing.T) {
	base := "repos:\n  - path: ~/r\nidentity:\n"
	if d, err := Parse([]byte(base + "  - .circleci\n  - .config/foo:rw\n  - gh\n")); err != nil || len(d.Identity) != 3 {
		t.Fatalf("valid identity = %+v, %v", d.Identity, err)
	}
	for _, bad := range []string{"/etc", "~/.aws", "../x", ".ssh", ".ssh/keys", ".gnupg", ".bough", ".bough/projects", "a//b", ".", ":rw", ".aws:ro", ".ssh:rw", "Library/LaunchAgents:rw", ".config:rw", ".config", ".config/fish:rw", ".config/gh", ".local/bin:rw", ".docker"} {
		if _, err := Parse([]byte(base + "  - " + bad + "\n")); err == nil || !strings.Contains(err.Error(), "identity") {
			t.Errorf("identity %q accepted (err %v)", bad, err)
		}
	}
}

func TestIdentityDir(t *testing.T) {
	for entry, want := range map[string]struct {
		dir string
		rw  bool
	}{".aws": {".aws", false}, ".kube:rw": {".kube", true}, "gh": {"", false}} {
		if d, rw := IdentityDir(entry); d != want.dir || rw != want.rw {
			t.Errorf("IdentityDir(%q) = %q, %v", entry, d, rw)
		}
	}
}
