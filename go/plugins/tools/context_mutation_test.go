package tools

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
)

// Context mutations borrow the installed native write policy. Checking
// the actual registration catches a permissive callback even when the
// underlying file helper still correctly rejects the same path.
func TestNativeContextWritePolicyMatchesNativeWrite(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"allowed", "outside", "parent-symlink", "leaf-symlink"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			root := filepath.Join(dir, "writable")
			outside := filepath.Join(dir, "outside")
			for _, path := range []string{root, outside} {
				if err := os.Mkdir(path, 0700); err != nil {
					t.Fatal(err)
				}
			}
			path := filepath.Join(root, "new", "notes.md")
			target := path
			wantBefore := ""
			switch name {
			case "outside":
				path = filepath.Join(outside, "notes.md")
				target = path
			case "parent-symlink":
				alias := filepath.Join(root, "escape")
				if err := os.Symlink(outside, alias); err != nil {
					t.Skipf("symlink unavailable: %v", err)
				}
				path = filepath.Join(alias, "new", "notes.md")
				target = filepath.Join(outside, "new", "notes.md")
			case "leaf-symlink":
				target = filepath.Join(outside, "notes.md")
				wantBefore = "untouched outside file"
				if err := os.WriteFile(target, []byte(wantBefore), 0600); err != nil {
					t.Fatal(err)
				}
				path = filepath.Join(root, "notes.md")
				if err := os.Symlink(target, path); err != nil {
					t.Skipf("symlink unavailable: %v", err)
				}
			}
			assertBefore := func() {
				t.Helper()
				b, err := os.ReadFile(target)
				if wantBefore == "" {
					if !os.IsNotExist(err) {
						t.Fatalf("policy check or denied write created %q: %q, %v", target, b, err)
					}
				} else if err != nil || string(b) != wantBefore {
					t.Fatalf("outside target changed: %q, %v", b, err)
				}
			}
			st := &Stats{writeRoots: []string{root}}
			var write agenttools.Tool
			for _, tool := range st.nativeTools(true) {
				if tool.Name == "write" {
					write = tool
				}
			}
			if write.WriteAllowed == nil || write.Call == nil {
				t.Fatal("native write did not provide its context mutation policy")
			}
			policyErr := write.WriteAllowed(t.Context(), path)
			assertBefore()
			args, err := json.Marshal(map[string]string{"path": path, "content": "authorized native write"})
			if err != nil {
				t.Fatal(err)
			}
			result, err := write.Call(t.Context(), agenttools.Call{Args: args})
			if err != nil {
				t.Fatal(err)
			}
			if name == "allowed" {
				if policyErr != nil || result.Error != "" {
					t.Fatalf("writable root refused: policy=%v, native=%q", policyErr, result.Error)
				}
				b, err := os.ReadFile(target)
				if err != nil || string(b) != "authorized native write" {
					t.Fatalf("allowed native write = %q, %v", b, err)
				}
				return
			}
			if policyErr == nil || !strings.Contains(policyErr.Error(), "outside this session's writable directories") {
				t.Fatalf("context policy did not reject escape: %v", policyErr)
			}
			if strings.TrimPrefix(policyErr.Error(), "context: ") != strings.TrimPrefix(result.Error, "write: ") {
				t.Fatalf("context policy and native write differ: %v / %q", policyErr, result.Error)
			}
			assertBefore()
		})
	}
}

func TestNativeReadOnlyToolsProvideNoContextWriteAuthority(t *testing.T) {
	t.Parallel()
	st := &Stats{writeRoots: []string{t.TempDir()}}
	for _, tool := range st.nativeTools(false) {
		if tool.Name == "write" || tool.Name == "patch" || tool.WriteAllowed != nil {
			t.Fatalf("read-only tool %q retained context write authority", tool.Name)
		}
	}
}

func mutationResult(t *testing.T, ch <-chan error) {
	t.Helper()
	select {
	case err := <-ch:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("native mutation did not finish; unrelated work or diagnostics held its lock")
	}
}

func TestNativeMutationUsesGuardedCanonicalPath(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	real := filepath.Join(dir, "real")
	if err := os.Mkdir(real, 0700); err != nil {
		t.Fatal(err)
	}
	alias := filepath.Join(dir, "alias")
	if err := os.Symlink(real, alias); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	path := filepath.Join(alias, "notes.md")
	canonical, _ := agenttools.CanonicalFile(filepath.Join(real, "notes.md"))
	var coordinated atomic.Int32
	ctx := agenttools.WithFileMutation(t.Context(), func(_ context.Context, got string, mutate func() error) error {
		coordinated.Add(1)
		if got != canonical {
			t.Errorf("coordination got %q, want canonical %q", got, canonical)
		}
		return mutate()
	})
	st := &Stats{writeRoots: []string{real}}
	if _, err := st.writeFile(ctx, path, "one"); err != nil {
		t.Fatal(err)
	}
	if _, err := st.patchFile(ctx, path, "one", "two"); err != nil {
		t.Fatal(err)
	}
	if coordinated.Load() != 2 {
		t.Fatalf("coordination calls %d, want 2", coordinated.Load())
	}
	outside := filepath.Join(dir, "outside.md")
	if _, err := st.writeFile(ctx, outside, "denied"); err == nil || !strings.Contains(err.Error(), "outside") {
		t.Fatalf("unguarded write: %v", err)
	}
	if coordinated.Load() != 2 {
		t.Fatal("denied path reached coordination before the write guard")
	}
	if _, err := os.Stat(outside); !os.IsNotExist(err) {
		t.Fatalf("denied write created a file: %v", err)
	}
}

func TestNativeMutationReleasesLockBeforeDiagnostics(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "notes.md")
	st := &Stats{}
	var mu sync.Mutex
	ctx := agenttools.WithFileMutation(t.Context(), func(_ context.Context, _ string, mutate func() error) error {
		mu.Lock()
		defer mu.Unlock()
		return mutate()
	})
	diagnostics := make(chan struct{})
	release := make(chan struct{})
	var releaseOnce sync.Once
	defer releaseOnce.Do(func() { close(release) })
	var edits atomic.Int32
	st.SetAfterEdit(func(string) string {
		if edits.Add(1) == 1 {
			close(diagnostics)
			<-release
		}
		return ""
	})
	first := make(chan error, 1)
	go func() { _, err := st.writeFile(ctx, path, "first"); first <- err }()
	select {
	case <-diagnostics:
	case <-time.After(5 * time.Second):
		t.Fatal("write did not reach diagnostics")
	}
	second := make(chan error, 1)
	go func() { _, err := st.patchFile(ctx, path, "first", "second"); second <- err }()
	mutationResult(t, second)
	releaseOnce.Do(func() { close(release) })
	mutationResult(t, first)
	b, err := os.ReadFile(path)
	if err != nil || string(b) != "second" {
		t.Fatalf("final file = %q, %v", b, err)
	}
}
