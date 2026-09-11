package tools

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

// filesKnown skips a subtest that pins a known product bug unless
// BOUGH_KNOWN_TOOLS_FILES=1.
func filesKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_FILES") != "1" {
		t.Skip("known bug (BOUGH_KNOWN_TOOLS_FILES=1 to run): " + bug)
	}
}

// filesUnchanged asserts path still holds want and the turn recorded
// no written file: a failed call must leave nothing partially applied
// and nothing for /undo to account for.
func filesUnchanged(t *testing.T, st *Stats, path string, want []byte) {
	t.Helper()
	got, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(got, want) {
		t.Fatalf("file changed by a failed call: %q %v, want %q", got, err, want)
	}
	if files, _, _ := st.Take(); len(files) != 0 {
		t.Fatalf("a failed call recorded written files: %v", files)
	}
}

func filesSeed(t *testing.T, path, content string) []byte {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	return []byte(content)
}

func filesNotRoot(t *testing.T) {
	t.Helper()
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("permission bits not enforced here")
	}
}

func TestFilesViewFailures(t *testing.T) {
	dir := t.TempDir()
	t.Run("missing", func(t *testing.T) {
		_, err := readView(filepath.Join(dir, "nope.txt"))
		if err == nil || !strings.Contains(err.Error(), "no such file") {
			t.Fatalf("err = %v", err)
		}
	})
	t.Run("directory lists entries", func(t *testing.T) {
		sub := filepath.Join(dir, "d")
		os.MkdirAll(filepath.Join(sub, "inner"), 0o755)
		filesSeed(t, filepath.Join(sub, "a.go"), "x")
		out, err := readView(sub)
		if err != nil || !strings.Contains(out, "is a directory") || !strings.Contains(out, "inner/") || !strings.Contains(out, "a.go") {
			t.Fatalf("view dir = %q %v", out, err)
		}
	})
	t.Run("no read permission", func(t *testing.T) {
		filesNotRoot(t)
		p := filepath.Join(dir, "secret")
		filesSeed(t, p, "s\n")
		os.Chmod(p, 0)
		t.Cleanup(func() { os.Chmod(p, 0o644) })
		if _, err := readView(p); err == nil || !strings.Contains(err.Error(), "permission denied") {
			t.Fatalf("err = %v", err)
		}
	})
	t.Run("symlink loop", func(t *testing.T) {
		a, b := filepath.Join(dir, "la"), filepath.Join(dir, "lb")
		os.Symlink(b, a)
		os.Symlink(a, b)
		done := make(chan error, 1)
		go func() { _, err := readView(a); done <- err }()
		select {
		case err := <-done:
			if err == nil || !strings.Contains(err.Error(), "symbolic links") {
				t.Fatalf("err = %v", err)
			}
		case <-time.After(5 * time.Second):
			t.Fatal("view hung on a symlink loop")
		}
	})
	t.Run("outside workspace is readable (no permissions by design)", func(t *testing.T) {
		other := t.TempDir()
		filesSeed(t, filepath.Join(other, "o.txt"), "outside\n")
		p := filepath.Join(other, "..", filepath.Base(other), "o.txt")
		if out, err := readView(p); err != nil || out != "1│outside\n" {
			t.Fatalf("view = %q %v", out, err)
		}
	})
	t.Run("empty file", func(t *testing.T) {
		p := filepath.Join(dir, "empty")
		filesSeed(t, p, "")
		if out, err := readView(p); err != nil || out != "1│\n" {
			t.Fatalf("view = %q %v", out, err)
		}
	})
	t.Run("inverted range", func(t *testing.T) {
		filesKnown(t, "view(path, 5, 2) returns \"\" and no error (tools.go readView: loop start..end silently empty)")
		p := filepath.Join(dir, "ten")
		filesSeed(t, p, strings.Repeat("l\n", 10))
		out, err := readView(p, 5, 2)
		if err == nil && out == "" {
			t.Fatal("inverted range returns an empty string with no error")
		}
	})
	t.Run("binary file", func(t *testing.T) {
		filesKnown(t, "view of a binary file returns raw NUL/control bytes numbered as text (tools.go readView: no binary check)")
		p := filepath.Join(dir, "bin.o")
		filesSeed(t, p, "\x7fELF\x00\x00\x01\x02\xff\xfe\x00tail")
		out, err := readView(p)
		if err == nil && strings.ContainsRune(out, 0) {
			t.Fatalf("binary bytes fed to the model: %q", out)
		}
	})
	t.Run("large file is capped", func(t *testing.T) {
		if os.Getenv("BOUGH_TOOLS_FILES_SLOW") != "1" {
			t.Skip("BOUGH_TOOLS_FILES_SLOW=1 writes a 50 MB file")
		}
		filesKnown(t, "view of a 50 MB file reads it whole and returns ~53 MB (tools.go readView: os.ReadFile, no size cap; only loop capOutput spills it after)")
		p := filepath.Join(dir, "big")
		filesSeed(t, p, strings.Repeat(strings.Repeat("x", 99)+"\n", 500_000))
		out, err := readView(p)
		if err == nil && len(out) > 1<<20 {
			t.Fatalf("view returned %d bytes for a 50 MB file", len(out))
		}
	})
}

func TestFilesWriteFailures(t *testing.T) {
	dir := t.TempDir()
	t.Run("read-only dir", func(t *testing.T) {
		filesNotRoot(t)
		ro := filepath.Join(dir, "ro")
		os.Mkdir(ro, 0o555)
		t.Cleanup(func() { os.Chmod(ro, 0o755) })
		st := &Stats{}
		if _, err := st.write(filepath.Join(ro, "f"), "x"); err == nil || !strings.Contains(err.Error(), "permission denied") {
			t.Fatalf("err = %v", err)
		}
		if files, _, _ := st.Take(); len(files) != 0 {
			t.Fatalf("failed write recorded %v", files)
		}
	})
	t.Run("over a directory", func(t *testing.T) {
		d := filepath.Join(dir, "isdir")
		os.MkdirAll(filepath.Join(d, "keep"), 0o755)
		st := &Stats{}
		if _, err := st.write(d, "x"); err == nil || !strings.Contains(err.Error(), "is a directory") {
			t.Fatalf("err = %v", err)
		}
		if _, err := os.Stat(filepath.Join(d, "keep")); err != nil {
			t.Fatal("directory contents lost")
		}
		if files, _, _ := st.Take(); len(files) != 0 {
			t.Fatalf("failed write recorded %v", files)
		}
	})
	t.Run("parent is a file", func(t *testing.T) {
		f := filepath.Join(dir, "plain")
		want := filesSeed(t, f, "keep")
		st := &Stats{}
		if _, err := st.write(filepath.Join(f, "child"), "x"); err == nil {
			t.Fatal("want an error")
		}
		filesUnchanged(t, st, f, want)
	})
	t.Run("missing parents created", func(t *testing.T) {
		p := filepath.Join(dir, "a", "b", "c", "d.txt")
		if _, err := (&Stats{}).write(p, "ok"); err != nil {
			t.Fatal(err)
		}
		if b, _ := os.ReadFile(p); string(b) != "ok" {
			t.Fatalf("content %q", b)
		}
	})
	t.Run("empty content", func(t *testing.T) {
		p := filepath.Join(dir, "empty.txt")
		out, err := (&Stats{}).write(p, "")
		if err != nil {
			t.Fatal(err)
		}
		if st, _ := os.Stat(p); st.Size() != 0 {
			t.Fatal("not empty")
		}
		filesKnown(t, "write reports \"0 bytes, 1 lines\" for empty content (and 3 lines for \"a\\nb\\n\") (tools.go write: strings.Count(content, \"\\n\")+1)")
		if strings.Contains(out, "1 lines") {
			t.Fatalf("write summary = %q", out)
		}
	})
	t.Run("huge content", func(t *testing.T) {
		p := filepath.Join(dir, "huge.txt")
		c := strings.Repeat("y", 8<<20)
		out, err := (&Stats{}).write(p, c)
		if err != nil || len(out) > 200 {
			t.Fatalf("write = %d bytes of summary, %v", len(out), err)
		}
		if st, _ := os.Stat(p); st.Size() != int64(len(c)) {
			t.Fatal("size mismatch")
		}
	})
	t.Run("CRLF preserved", func(t *testing.T) {
		p := filepath.Join(dir, "crlf.txt")
		if _, err := (&Stats{}).write(p, "a\r\nb\r\n"); err != nil {
			t.Fatal(err)
		}
		if b, _ := os.ReadFile(p); string(b) != "a\r\nb\r\n" {
			t.Fatalf("content %q", b)
		}
	})
	t.Run("keeps executable bit on overwrite", func(t *testing.T) {
		p := filepath.Join(dir, "run.sh")
		filesSeed(t, p, "#!/bin/sh\n")
		os.Chmod(p, 0o755)
		if _, err := (&Stats{}).write(p, "#!/bin/sh\necho\n"); err != nil {
			t.Fatal(err)
		}
		if st, _ := os.Stat(p); st.Mode().Perm()&0o100 == 0 {
			t.Fatalf("mode = %v", st.Mode())
		}
	})
}

func TestFilesPatchFailures(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "f.txt")
	orig := "alpha\nbeta\ngamma\nbeta\n"
	t.Run("hunk does not apply", func(t *testing.T) {
		want := filesSeed(t, p, orig)
		st := &Stats{}
		if _, err := st.patch(p, "delta\n", "x"); err == nil || !strings.Contains(err.Error(), "not found") {
			t.Fatalf("err = %v", err)
		}
		filesUnchanged(t, st, p, want)
	})
	t.Run("ambiguous", func(t *testing.T) {
		want := filesSeed(t, p, orig)
		st := &Stats{}
		if _, err := st.patch(p, "beta\n", "x"); err == nil || !strings.Contains(err.Error(), "occurs 2 times") {
			t.Fatalf("err = %v", err)
		}
		filesUnchanged(t, st, p, want)
	})
	t.Run("stale: changed on disk since view", func(t *testing.T) {
		filesSeed(t, p, orig)
		st := &Stats{}
		if _, err := st.view(p); err != nil {
			t.Fatal(err)
		}
		want := filesSeed(t, p, "ALPHA\nbeta\ngamma\n") // edited elsewhere
		if _, err := st.patch(p, "alpha\nbeta\n", "x\n"); err == nil || !strings.Contains(err.Error(), "closest match") {
			t.Fatalf("err = %v", err)
		}
		filesUnchanged(t, st, p, want)
		// An old that still matches applies to the CURRENT file.
		if _, err := st.patch(p, "gamma\n", "G\n"); err != nil {
			t.Fatal(err)
		}
		if b, _ := os.ReadFile(p); string(b) != "ALPHA\nbeta\nG\n" {
			t.Fatalf("content %q", b)
		}
	})
	t.Run("missing file", func(t *testing.T) {
		st := &Stats{}
		if _, err := st.patch(filepath.Join(dir, "gone.txt"), "a", "b"); err == nil || !strings.Contains(err.Error(), "no such file") {
			t.Fatalf("err = %v", err)
		}
		if _, err := os.Stat(filepath.Join(dir, "gone.txt")); err == nil {
			t.Fatal("failed patch created the file")
		}
	})
	t.Run("directory", func(t *testing.T) {
		st := &Stats{}
		for _, old := range []string{"", "a"} {
			if _, err := st.patch(dir, old, "b"); err == nil {
				t.Fatalf("patch(dir, %q) succeeded", old)
			}
		}
	})
	t.Run("unified-diff syntax is literal text", func(t *testing.T) {
		want := filesSeed(t, p, orig)
		st := &Stats{}
		_, err := st.patch(p, "@@ -1,1 +1,1 @@\n-alpha\n+ALPHA\n", "")
		if err == nil || !strings.Contains(err.Error(), "not found") {
			t.Fatalf("err = %v", err)
		}
		filesUnchanged(t, st, p, want)
	})
	t.Run("read-only file", func(t *testing.T) {
		filesNotRoot(t)
		want := filesSeed(t, p, orig)
		os.Chmod(p, 0o444)
		t.Cleanup(func() { os.Chmod(p, 0o644) })
		st := &Stats{}
		if _, err := st.patch(p, "alpha", "x"); err == nil || !strings.Contains(err.Error(), "permission denied") {
			t.Fatalf("err = %v", err)
		}
		os.Chmod(p, 0o644)
		filesUnchanged(t, st, p, want)
	})
	t.Run("CRLF file keeps CRLF on patch", func(t *testing.T) {
		q := filepath.Join(dir, "crlf.txt")
		filesSeed(t, q, "one\r\ntwo\r\nthree\r\n")
		if _, err := (&Stats{}).patch(q, "two", "TWO"); err != nil {
			t.Fatal(err)
		}
		if b, _ := os.ReadFile(q); string(b) != "one\r\nTWO\r\nthree\r\n" {
			t.Fatalf("content %q", b)
		}
	})
	t.Run("CRLF file: multi-line old copied from view", func(t *testing.T) {
		filesKnown(t, "patch on a CRLF file with a multi-line old copied from view (LF) never matches; view hides the \\r (tools.go patch: exact-byte strings.Count)")
		q := filepath.Join(dir, "crlf2.txt")
		filesSeed(t, q, "one\r\ntwo\r\nthree\r\n")
		if _, err := (&Stats{}).patch(q, "one\ntwo", "ONE\nTWO"); err != nil {
			t.Fatalf("patch = %v", err)
		}
	})
	t.Run("identical content", func(t *testing.T) {
		filesKnown(t, "patch with old == new reports \"patched (+0 lines)\" and records the file as written (tools.go patch: no no-op check)")
		want := filesSeed(t, p, orig)
		st := &Stats{}
		out, err := st.patch(p, "alpha", "alpha")
		if err == nil && !strings.Contains(out, "no change") {
			t.Fatalf("no-op patch = %q", out)
		}
		filesUnchanged(t, st, p, want)
	})
}

// Two subagents (two Stats: each worker has its own codemode) patch
// different lines of one file at once: neither edit may be lost.
func TestFilesConcurrentPatchesFromTwoAgents(t *testing.T) {
	for round := range 50 {
		p := filepath.Join(t.TempDir(), "shared.txt")
		var lines []string
		for i := range 20 {
			lines = append(lines, fmt.Sprintf("line%02d", i))
		}
		filesSeed(t, p, strings.Join(lines, "\n")+"\n")
		var wg sync.WaitGroup
		for a := range 2 {
			wg.Go(func() {
				st := &Stats{}
				for i := a; i < 20; i += 2 {
					old := fmt.Sprintf("line%02d\n", i)
					st.patch(p, old, strings.ToUpper(old))
				}
			})
		}
		wg.Wait()
		b, _ := os.ReadFile(p)
		if n := strings.Count(string(b), "LINE"); n != 20 {
			t.Fatalf("round %d: %d of 20 edits survived:\n%s", round, n, b)
		}
	}
}

// Through codemode: every failure reaches the script as a catchable
// error carrying the tool's message, and the runtime stays usable.
func TestFilesFailuresThroughCodemode(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	cm, _ := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	dir := t.TempDir()
	p := filepath.Join(dir, "f.txt")
	filesSeed(t, p, "a\na\n")
	for _, c := range []struct{ js, want string }{
		{`tools.view(` + jsStr(dir+"/missing.txt") + `)`, "no such file"},
		{`tools.patch(` + jsStr(p) + `, "a", "b")`, "occurs 2 times"},
		{`tools.patch(` + jsStr(p) + `, "zz", "b")`, "not found"},
		{`tools.write(` + jsStr(dir) + `, "x")`, "is a directory"},
	} {
		out, err := cm.Run(`try { ` + c.js + `; "no error" } catch (e) { "caught: " + e }`)
		if err != nil || !strings.Contains(out, "caught:") || !strings.Contains(out, c.want) {
			t.Errorf("%s = %q %v, want caught %q", c.js, out, err, c.want)
		}
		if _, err := cm.Run(c.js); err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("uncaught %s: err = %v", c.js, err)
		}
	}
	if out, err := cm.Run(`tools.view(` + jsStr(p) + `)`); err != nil || out != "1│a\n2│a\n" {
		t.Fatalf("runtime unusable after failures: %q %v", out, err)
	}
	// Wrong argument types do not panic the host.
	for _, js := range []string{`tools.view()`, `tools.patch(1)`, `tools.write(null, {})`} {
		if _, err := cm.Run(`try { ` + js + ` } catch (e) { "caught" }`); err != nil {
			t.Errorf("%s: %v", js, err)
		}
	}
}
