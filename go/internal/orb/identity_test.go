package orb

import (
	"os"
	"path/filepath"
	"testing"
)

func TestIdentityMountsProjectExtra(t *testing.T) {
	home := t.TempDir()
	for _, d := range []string{".circleci", ".config/foo", ".aws"} {
		if err := os.MkdirAll(filepath.Join(home, d), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// .aws is built in and also listed; .missing does not exist on the host.
	ms := identityMounts(home, []string{".config/foo", ".aws", ".missing"})
	got := map[string]int{}
	for _, m := range ms {
		got[m.Target]++
		if m.ReadOnly || m.Source != filepath.Join(home, m.Target[len("/root/"):]) {
			t.Errorf("mount %+v: want read-write from the same $HOME path", m)
		}
	}
	for _, want := range []string{"/root/.circleci", "/root/.config/foo", "/root/.aws"} {
		if got[want] != 1 {
			t.Errorf("%s mounted %d times, want 1 (mounts %+v)", want, got[want], ms)
		}
	}
	if got["/root/.missing"] != 0 {
		t.Errorf("a dir missing on the host was mounted: %+v", ms)
	}
}
