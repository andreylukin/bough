package projectdef

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// MEMORY.md is the project's brief, not a build input: an edit to it must
// not retag the image, because orb.Open removes and recreates the
// container on a new tag. The Dockerfile path is the one that used to
// hash the whole project dir, including a Dockerfile that merely mentions
// the file.
func TestImageHashIgnoresMemory(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := Create(home, "h"); err != nil {
		t.Fatal(err)
	}
	if err := WriteFile(home, "h", FileYAML, "repos:\n  - path: "+home+"\n"); err != nil {
		t.Fatal(err)
	}
	hash := func() string {
		t.Helper()
		p, err := Load(home, "h")
		if err != nil {
			t.Fatal(err)
		}
		s, err := ImageHash(home, p)
		if err != nil {
			t.Fatal(err)
		}
		return s
	}
	for _, df := range []string{"", "FROM debian\nCOPY . .\n", "FROM debian\n# the brief lives in MEMORY.md\nCOPY setup.sh /\n"} {
		if err := WriteFile(home, "h", FileDockerfile, df); err != nil {
			t.Fatal(err)
		}
		h0 := hash()
		for _, text := range []string{"one line\n", "", "a much longer brief\nover two lines\n"} {
			if err := WriteFile(home, "h", FileMemory, text); err != nil {
				t.Fatal(err)
			}
			if h := hash(); h != h0 {
				t.Fatalf("Dockerfile %q: MEMORY.md moved the hash %s -> %s", df, h0, h)
			}
		}
		os.Remove(filepath.Join(Root(home), "h", FileMemory))
		if h := hash(); h != h0 {
			t.Fatalf("Dockerfile %q: removing MEMORY.md moved the hash", df)
		}
	}
}

// A definition with no repos and no build script builds on the base
// image; hashing and tagging it must work, or it can never be opened.
func TestImageHashNoBuildScript(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p, err := CreateEmpty(home, "lab", "Lab notes")
	if err != nil {
		t.Fatal(err)
	}
	h, err := ImageHash(home, p)
	if err != nil || len(h) != 12 {
		t.Fatalf("ImageHash = %q, %v", h, err)
	}
	if again, _ := ImageHash(home, p); again != h {
		t.Fatal("unstable")
	}
	if tag := ImageTag(p.Slug, h); tag != "bough-orb/lab:"+h || strings.Contains(tag, " ") {
		t.Fatalf("tag = %q", tag)
	}
	// The brief is not a build input here either.
	if err := WriteFile(home, "lab", FileMemory, "what this project is\n"); err != nil {
		t.Fatal(err)
	}
	if again, _ := ImageHash(home, p); again != h {
		t.Fatal("MEMORY.md moved the hash of a script-less project")
	}
}
