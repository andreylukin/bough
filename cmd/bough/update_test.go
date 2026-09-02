package main

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func mkdirs(t *testing.T, dirs ...string) {
	t.Helper()
	for _, d := range dirs {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
}

func touch(t *testing.T, path string) {
	t.Helper()
	mkdirs(t, filepath.Dir(path))
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestFindCheckoutWalksUpFromExe(t *testing.T) {
	root := t.TempDir()
	mkdirs(t, filepath.Join(root, ".git"))
	exe := filepath.Join(root, "go", "bough")
	touch(t, exe)

	got, err := findCheckout(exe, "", filepath.Join(t.TempDir(), "nohome"))
	if err != nil {
		t.Fatal(err)
	}
	if got != root {
		t.Fatalf("got %q, want %q", got, root)
	}
}

func TestFindCheckoutBoughRoot(t *testing.T) {
	exe := filepath.Join(t.TempDir(), "bin", "bough") // no .git anywhere above
	touch(t, exe)
	root := t.TempDir()
	mkdirs(t, filepath.Join(root, ".git"))

	got, err := findCheckout(exe, root, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if got != root {
		t.Fatalf("got %q, want %q", got, root)
	}

	// A set-but-bad BOUGH_ROOT fails loud instead of falling through.
	bad := t.TempDir()
	if _, err := findCheckout(exe, bad, t.TempDir()); err == nil || !strings.Contains(err.Error(), "BOUGH_ROOT") {
		t.Fatalf("want BOUGH_ROOT error, got %v", err)
	}
}

func TestFindCheckoutHomeFallback(t *testing.T) {
	exe := filepath.Join(t.TempDir(), "bin", "bough")
	touch(t, exe)
	home := t.TempDir()
	def := filepath.Join(home, "repos", "bough")
	mkdirs(t, filepath.Join(def, ".git"))

	got, err := findCheckout(exe, "", home)
	if err != nil {
		t.Fatal(err)
	}
	if got != def {
		t.Fatalf("got %q, want %q", got, def)
	}
}

func TestFindCheckoutFailsNamingTried(t *testing.T) {
	exe := filepath.Join(t.TempDir(), "bin", "bough")
	touch(t, exe)
	home := t.TempDir()

	_, err := findCheckout(exe, "", home)
	if err == nil {
		t.Fatal("want error")
	}
	for _, want := range []string{exe, "$BOUGH_ROOT", filepath.Join(home, "repos", "bough")} {
		if !strings.Contains(err.Error(), want) {
			t.Fatalf("error %q missing %q", err, want)
		}
	}
}

func TestModuleDir(t *testing.T) {
	merged := t.TempDir() // post-merge: <root>/go/go.mod
	touch(t, filepath.Join(merged, "go", "go.mod"))
	if got, err := moduleDir(merged); err != nil || got != filepath.Join(merged, "go") {
		t.Fatalf("merged layout: got %q, %v", got, err)
	}

	plain := t.TempDir() // current layout: <root>/go.mod
	touch(t, filepath.Join(plain, "go.mod"))
	if got, err := moduleDir(plain); err != nil || got != plain {
		t.Fatalf("plain layout: got %q, %v", got, err)
	}

	if _, err := moduleDir(t.TempDir()); err == nil {
		t.Fatal("want error for no go.mod")
	}
}

func TestInstallTarget(t *testing.T) {
	root := t.TempDir()
	modDir := filepath.Join(root, "go")

	// In-checkout builds go to <modDir>/bough.
	for _, exe := range []string{
		filepath.Join(modDir, "bough"),
		filepath.Join(root, "target", "release", "bough"),
	} {
		got, installed := installTarget(exe, root, modDir)
		if installed || got != filepath.Join(modDir, "bough") {
			t.Fatalf("exe %q: got %q installed=%v", exe, got, installed)
		}
	}

	// An installed copy is overwritten in place.
	exe := filepath.Join(t.TempDir(), ".local", "bin", "bough")
	got, installed := installTarget(exe, root, modDir)
	if !installed || got != exe {
		t.Fatalf("installed exe: got %q installed=%v", got, installed)
	}
}

// deadPid returns the pid of a process that has already exited.
func deadPid(t *testing.T) int {
	t.Helper()
	cmd := exec.Command("true")
	if err := cmd.Run(); err != nil {
		t.Fatal(err)
	}
	return cmd.Process.Pid
}

func TestRestartWebNoPidfile(t *testing.T) {
	var out bytes.Buffer
	if err := restartWeb(t.TempDir(), "/bin/echo", &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "no running web session") {
		t.Fatalf("output %q", out.String())
	}
}

func TestRestartWebStalePidfile(t *testing.T) {
	home := t.TempDir()
	pf := webPidfile(home)
	touch(t, pf)
	if err := os.WriteFile(pf, []byte(strconv.Itoa(deadPid(t))+" localhost:7681\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := restartWeb(home, "/bin/echo", &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "no running web session") {
		t.Fatalf("output %q", out.String())
	}
	if _, err := os.Stat(pf); !os.IsNotExist(err) {
		t.Fatalf("stale pidfile not removed: %v", err)
	}
}

func TestRestartWebMalformedPidfile(t *testing.T) {
	home := t.TempDir()
	pf := webPidfile(home)
	touch(t, pf)
	if err := os.WriteFile(pf, []byte("garbage\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	var out bytes.Buffer
	if err := restartWeb(home, "/bin/echo", &out); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "no running web session") {
		t.Fatalf("output %q", out.String())
	}
	if _, err := os.Stat(pf); !os.IsNotExist(err) {
		t.Fatalf("malformed pidfile not removed: %v", err)
	}
}
