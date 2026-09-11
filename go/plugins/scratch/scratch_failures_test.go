package scratch

// Failure-path tests for tools.scratch through the real codemode VM:
// escaping the scratch dir, a missing / removed session dir, an
// unwritable dir (the disk-full stand-in), and values JSON cannot hold.
// Not parallel: Apply sets $BOUGH_SCRATCH process-wide.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	cmpkg "github.com/andreylukin/bough/plugins/codemode"
)

func scratchFailMount(t *testing.T, dir string) (*cmpkg.CodeMode, *Pad) {
	t.Helper()
	t.Setenv("BOUGH_SCRATCH", "") // restored after the test
	cm := cmpkg.New(5 * time.Second)
	ctx := kernel.NewContext()
	ctx.Provide("codemode", cm)
	if err := (plugin{}).Apply(ctx, map[string]any{"dir": dir}); err != nil {
		t.Fatalf("apply: %v", err)
	}
	pad, err := kernel.Get[*Pad](ctx, "scratch")
	if err != nil {
		t.Fatal(err)
	}
	return cm, pad
}

func TestScratchFailuresFileStaysInside(t *testing.T) {
	root := t.TempDir()
	dir := filepath.Join(root, "pad")
	cm, _ := scratchFailMount(t, dir)
	for _, name := range []string{"../../etc/passwd", "/etc/passwd", "a/../../../x", "./../sibling", "~/.ssh/id", "a/./b/../c"} {
		out, err := cm.Run(`tools.scratch.file(` + jsq(name) + `)`)
		if err != nil {
			t.Errorf("%q: %v", name, err)
			continue
		}
		rel, rerr := filepath.Rel(dir, out)
		if rerr != nil || rel == ".." || strings.HasPrefix(rel, "../") || !filepath.IsAbs(out) {
			t.Errorf("file(%q) = %q escapes %s", name, out, dir)
		}
	}
	for _, name := range []string{"", "   ", "/", ".", ".."} {
		if out, err := cm.Run(`tools.scratch.file(` + jsq(name) + `)`); err == nil {
			t.Errorf("file(%q) = %q, want error", name, out)
		}
	}
	// Nothing was created outside the pad.
	ents, _ := os.ReadDir(root)
	for _, e := range ents {
		if e.Name() != "pad" {
			t.Errorf("stray entry outside the pad: %s", e.Name())
		}
	}
}

// A session dir that does not exist yet (fresh session) and one removed
// mid-session are both recreated on the next write.
func TestScratchFailuresMissingSessionDir(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "never", "made")
	cm, _ := scratchFailMount(t, dir)
	for _, code := range []string{`tools.scratch.keys().length`, `tools.scratch.notes()`, `tools.scratch.list()`} {
		if _, err := cm.Run(code); err != nil {
			t.Fatalf("%s on a missing dir: %v", code, err)
		}
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatal("reads created the dir")
	}
	if _, err := cm.Run(`tools.scratch.set("k", {a: [1, "two"]}); tools.scratch.note("n1")`); err != nil {
		t.Fatal(err)
	}
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	out, err := cm.Run(`tools.scratch.note("after rm"); tools.scratch.set("k2", 1); JSON.stringify(tools.scratch.get("k"))`)
	if err != nil || out != `{"a":[1,"two"]}` {
		t.Fatalf("after rm: %q, %v", out, err)
	}
	if b, _ := os.ReadFile(filepath.Join(dir, stateFile)); !strings.Contains(string(b), `"k2"`) || !strings.Contains(string(b), `"k"`) {
		t.Fatalf("state not rewritten: %s", b)
	}
}

// The pad path is blocked (a regular file sits where the dir goes) or
// the dir is read-only: the disk-full stand-in. Every write throws a
// catchable error naming scratch; reads keep working; nothing panics.
func TestScratchFailuresUnwritable(t *testing.T) {
	t.Run("path is a file", func(t *testing.T) {
		dir := filepath.Join(t.TempDir(), "pad")
		if err := os.WriteFile(dir, []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
		cm, _ := scratchFailMount(t, dir)
		scratchFailAllWritesThrow(t, cm)
	})
	t.Run("read-only dir", func(t *testing.T) {
		if os.Geteuid() == 0 {
			t.Skip("root ignores directory permissions")
		}
		dir := filepath.Join(t.TempDir(), "pad")
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(dir, 0o500); err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { os.Chmod(dir, 0o755) })
		cm, _ := scratchFailMount(t, dir)
		scratchFailAllWritesThrow(t, cm)
	})
}

func scratchFailAllWritesThrow(t *testing.T, cm *cmpkg.CodeMode) {
	t.Helper()
	for _, code := range []string{`tools.scratch.set("k", 1)`, `tools.scratch.note("hi")`, `tools.scratch.file("sub/probe.sh")`} {
		out, err := cm.Run(`try { ` + code + `; "no error" } catch (e) { "caught: " + e.message }`)
		if err != nil {
			t.Fatalf("%s: uncaught %v", code, err)
		}
		if !strings.Contains(out, "caught:") || !strings.Contains(out, "scratch") {
			t.Errorf("%s: %q, want a caught scratch error", code, out)
		}
	}
	for _, code := range []string{`tools.scratch.keys()`, `tools.scratch.notes()`, `tools.scratch.list()`, `tools.scratch.dir()`} {
		if _, err := cm.Run(code); err != nil {
			t.Errorf("%s: %v", code, err)
		}
	}
}

// A failed save must not leave the value visible as if it were stored:
// set() threw, so get() should not return it.
func TestScratchFailuresFailedSetNotStored(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "pad")
	if err := os.WriteFile(dir, []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	cm, _ := scratchFailMount(t, dir)
	out, err := cm.Run(`try { tools.scratch.set("k", 1) } catch (e) {}; try { "got " + tools.scratch.get("k") } catch (e) { "absent" }`)
	if err != nil || out != "absent" {
		t.Fatalf("after failed set: %q, %v", out, err)
	}
}

// Values JSON cannot represent are refused with a catchable error and
// leave the pad unchanged; a cyclic object must not hang or crash.
func TestScratchFailuresBadValues(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "pad")
	cm, pad := scratchFailMount(t, dir)
	for _, code := range []string{
		`var o = {}; o.self = o; tools.scratch.set("cyc", o)`,
		`tools.scratch.set("", 1)`,
		`tools.scratch.set("big", "x".repeat(2 << 20))`,
	} {
		done := make(chan struct{})
		var out string
		var err error
		go func() {
			defer close(done)
			out, err = cm.Run(`try { ` + code + `; "stored" } catch (e) { "caught: " + e.message }`)
		}()
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			t.Fatalf("%s hung", code)
		}
		t.Logf("%s -> %q, %v", code, out, err)
		if err != nil {
			t.Errorf("%s: uncaught %v", code, err)
		}
		if out == "stored" {
			t.Errorf("%s was stored", code)
		}
	}
	if keys := pad.Keys(); len(keys) != 0 {
		t.Fatalf("refused values left keys: %v", keys)
	}
	// function / undefined values: whatever is stored must reload.
	cm.Run(`try { tools.scratch.set("fn", function(){}) } catch (e) {}; try { tools.scratch.set("u", undefined) } catch (e) {}`)
	again, _ := New(dir)
	if len(again.Keys()) != len(pad.Keys()) {
		t.Fatalf("in-memory keys %v vs reloaded %v", pad.Keys(), again.Keys())
	}
}

// A corrupt state file starts empty rather than failing the mount, and
// the next set overwrites it with valid JSON.
func TestScratchFailuresCorruptState(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "pad")
	os.MkdirAll(dir, 0o755)
	os.WriteFile(filepath.Join(dir, stateFile), []byte("{not json"), 0o644)
	cm, _ := scratchFailMount(t, dir)
	if out, err := cm.Run(`tools.scratch.set("k", 2); tools.scratch.get("k")`); err != nil || out != "2" {
		t.Fatalf("%q, %v", out, err)
	}
	again, _ := New(dir)
	if v, err := again.Get("k"); err != nil || v != 2.0 {
		t.Fatalf("reloaded %v, %v", v, err)
	}
}

func jsq(s string) string {
	return `"` + strings.NewReplacer(`\`, `\\`, `"`, `\"`).Replace(s) + `"`
}
