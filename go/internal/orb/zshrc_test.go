package orb

import (
	"context"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
)

// Tests never read the developer's ~/.zshrc.
func TestMain(m *testing.M) {
	hostShellEnv = func() map[string]string { return nil }
	os.Exit(m.Run())
}

func TestShellEnvDiff(t *testing.T) {
	rc := map[string]string{
		"SHLVL":       "1",
		"NOTION_KEY":  "ntn_abc12345",
		"CHANGED":     "rc",
		"PATH":        "/usr/bin",
		"AWS_PROFILE": "dev",
		"NVM_DIR":     "/Users/me/.nvm",
		"BREW":        "/opt/homebrew/opt/x",
		"PAGER":       "less",
	}
	base := map[string]string{"SHLVL": "1", "CHANGED": "base"}
	got := shellEnvDiff(rc, base, "/Users/me")
	want := map[string]string{"NOTION_KEY": "ntn_abc12345", "CHANGED": "rc"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %v, want %v", got, want)
	}
}

// zshrc exports reach execs as secrets and are redacted; project env and
// resolved secrets of the same name win.
func TestOrbShellEnv(t *testing.T) {
	oldRead, oldShell := secrets.KeychainRead, hostShellEnv
	t.Cleanup(func() {
		secrets.KeychainRead, hostShellEnv = oldRead, oldShell
		shellEnvCache.at = time.Time{}
	})
	secrets.KeychainRead = func(service string) (string, error) {
		if service == "bough/zrc/API_KEY" {
			return "sk-live-0123456789", nil
		}
		return "", secrets.ErrNotFound
	}
	hostShellEnv = func() map[string]string {
		return map[string]string{"NOTION_KEY": "ntn-0123456789", "API_KEY": "from-zshrc-0000", "GOFLAGS": "-from-zshrc"}
	}
	shellEnvCache.at = time.Time{}
	ctx := context.Background()
	home, scratch := t.TempDir(), t.TempDir()
	newProject(t, home, "zrc", "  - path: "+newRepo(t)+"\n")
	projectdef.WriteFile(home, "zrc", projectdef.FileResume, "#!/bin/sh\necho \"n=$NOTION_KEY k=$API_KEY g=$GOFLAGS\"\n")
	if err := projectdef.SetSecret(home, "zrc", "API_KEY", "keychain:bough/zrc/API_KEY"); err != nil {
		t.Fatal(err)
	}
	y, _ := projectdef.ReadFile(home, "zrc", projectdef.FileYAML)
	if err := projectdef.WriteFile(home, "zrc", projectdef.FileYAML, y+"env:\n  GOFLAGS: -mod=mod\n"); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(home, "zrc")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Open(ctx, container.NewFake(), home, "s-zrc", p, scratch); err != nil {
		t.Fatal(err)
	}
	log, _ := os.ReadFile(filepath.Join(Dir(home, "s-zrc"), "resume.log"))
	if !strings.Contains(string(log), "n=[redacted:NOTION_KEY] k=[redacted:API_KEY] g=-mod=mod") {
		t.Fatalf("resume.log %q", log)
	}
}
