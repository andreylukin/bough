package projectdef

import (
	"strings"
	"testing"
)

func TestParseIdentity(t *testing.T) {
	base := "repos:\n  - path: ~/r\nidentity:\n"
	if d, err := Parse([]byte(base + "  - .circleci\n  - .config/foo\n")); err != nil || len(d.Identity) != 2 {
		t.Fatalf("valid identity = %+v, %v", d.Identity, err)
	}
	for _, bad := range []string{"/etc", "~/.aws", "../x", ".ssh", ".ssh/keys", ".gnupg", ".bough", ".bough/projects", "a//b", "."} {
		if _, err := Parse([]byte(base + "  - " + bad + "\n")); err == nil || !strings.Contains(err.Error(), "identity") {
			t.Errorf("identity %q accepted (err %v)", bad, err)
		}
	}
}
