package orb

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

// A project session's history never holds a resolved secret value: the
// orb row installs its redactor on the history store. Not parallel: it
// swaps the keychain seam and userHome.
func TestProjectHistoryRedactsSecrets(t *testing.T) {
	old := secrets.KeychainRead
	t.Cleanup(func() { secrets.KeychainRead = old })
	secrets.KeychainRead = func(service string) (string, error) {
		if service == "bough/red/API_KEY" {
			return "sk-live-0123456789", nil
		}
		return "", secrets.ErrNotFound
	}
	home := t.TempDir()
	repo := filepath.Join(t.TempDir(), "app")
	os.MkdirAll(repo, 0o755)
	for _, args := range [][]string{{"init", "-q", "-b", "main"}, {"-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"}} {
		if out, err := exec.Command("git", append([]string{"-C", repo}, args...)...).CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	if _, err := projectdef.Create(home, "red"); err != nil {
		t.Fatal(err)
	}
	if err := projectdef.WriteFile(home, "red", projectdef.FileYAML, "repos:\n  - path: "+repo+"\n"); err != nil {
		t.Fatal(err)
	}
	if err := projectdef.SetSecret(home, "red", "API_KEY", "keychain:bough/red/API_KEY"); err != nil {
		t.Fatal(err)
	}
	oldHome, oldChdir := userHome, chdir
	t.Cleanup(func() { userHome, chdir = oldHome, oldChdir })
	userHome = func() (string, error) { return home, nil }
	chdir = func(string) error { return nil }

	ctx := kernel.NewContext()
	ctx.Provide("session-mode", "project")
	ctx.Provide("session-project", "red")
	ctx.Provide("codemode", codemode.New(5*time.Second))
	hist := filepath.Join(home, ".bough", "history", "sred.jsonl")
	rows := []kernel.Row{
		{ID: "history", Plugin: "history", Config: map[string]any{"file": hist}},
		{ID: "scratchpad", Plugin: "scratchpad", Config: map[string]any{"dir": filepath.Join(home, "scratch")}},
		{ID: "orb", Plugin: "orb", Config: map[string]any{"runtime": "fake"}},
	}
	if err := ctx.Mount(rows); err != nil {
		t.Fatal(err)
	}
	defer ctx.Unmount()
	if _, err := kernel.Get[any](ctx, "orb"); err != nil {
		t.Fatalf("orb: %v", err)
	}
	rec, err := kernel.Get[func(string, map[string]any)](ctx, "history-record")
	if err != nil {
		t.Fatal(err)
	}
	rec("exec", map[string]any{"output": "token sk-live-0123456789"})
	b, _ := os.ReadFile(hist)
	if strings.Contains(string(b), "sk-live") || !strings.Contains(string(b), "[redacted:API_KEY]") {
		t.Fatalf("history:\n%s", b)
	}
}
