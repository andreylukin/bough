package orb

import (
	"context"
	"os"
	"path/filepath"

	"bytes"
	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"strings"
	"testing"
)

func TestRedactString(t *testing.T) {
	r := NewRedactor(map[string]string{"TOKEN": "s3cr3t-value", "SHORT": "abc1234", "LONG": "s3cr3t-value-longer"})
	got := r.String("a s3cr3t-value b s3cr3t-value-longer c abc1234")
	want := "a [redacted:TOKEN] b [redacted:LONG] c abc1234"
	if got != want {
		t.Fatalf("got %q want %q", got, want)
	}
	if NewRedactor(nil) != nil {
		t.Fatal("no secrets: nil redactor")
	}
	var nr *Redactor
	if nr.String("x") != "x" {
		t.Fatal("nil redactor passes through")
	}
}

// Every split of the stream into two or three chunks must redact alike,
// including splits inside the secret and a trailing partial prefix.
func TestRedactWriterChunks(t *testing.T) {
	r := NewRedactor(map[string]string{"K": "hunter2hunter2", "IN": "hunter2hunter2-and-more"})
	in := "pre hunter2hunter2 mid hunter2hunt end hunter2hunter2-and-more x hunter2hunter2"
	want := r.String(in)
	if strings.Contains(want, "hunter2hunter2") {
		t.Fatalf("String missed: %q", want)
	}
	for i := 0; i <= len(in); i++ {
		for j := i; j <= len(in); j++ {
			var b bytes.Buffer
			w := r.Writer(&b)
			for _, c := range []string{in[:i], in[i:j], in[j:]} {
				if n, err := w.Write([]byte(c)); err != nil || n != len(c) {
					t.Fatalf("write %d %v", n, err)
				}
			}
			w.Close()
			if b.String() != want {
				t.Fatalf("split %d,%d: got %q want %q", i, j, b.String(), want)
			}
		}
	}
	// A held partial prefix that never completes is flushed raw on Close.
	var b bytes.Buffer
	w := r.Writer(&b)
	w.Write([]byte("tail hunter2"))
	if strings.Contains(b.String(), "hunter2") {
		t.Fatalf("partial prefix emitted early: %q", b.String())
	}
	w.Close()
	if b.String() != "tail hunter2" {
		t.Fatalf("close: %q", b.String())
	}
}

// Resolved secrets are redacted in resume.log and by the orb's Redactor;
// `redact: false` in project.yml turns both off. The keychain is
// TestMain's.
func TestOrbRedactsSecrets(t *testing.T) {
	t.Parallel()
	for _, optOut := range []bool{false, true} {
		ctx := context.Background()
		home, scratch := t.TempDir(), t.TempDir()
		newProject(t, home, "red", "  - path: "+newRepo(t)+"\n")
		projectdef.WriteFile(home, "red", projectdef.FileResume, "#!/bin/sh\necho \"key=$API_KEY port=$PORT\"\n")
		for name, ref := range map[string]string{"API_KEY": "keychain:bough/red/API_KEY", "PORT": "keychain:bough/red/PORT"} {
			if err := projectdef.SetSecret(home, "red", name, ref); err != nil {
				t.Fatal(err)
			}
		}
		if optOut {
			y, _ := projectdef.ReadFile(home, "red", projectdef.FileYAML)
			if err := projectdef.WriteFile(home, "red", projectdef.FileYAML, y+"redact: false\n"); err != nil {
				t.Fatal(err)
			}
		}
		p, err := projectdef.Load(home, "red")
		if err != nil {
			t.Fatal(err)
		}
		o, err := Open(ctx, container.NewFake(), home, "s-red", p, scratch)
		if err != nil {
			t.Fatal(err)
		}
		log, _ := os.ReadFile(filepath.Join(Dir(home, "s-red"), "resume.log"))
		got := o.Redact("out sk-live-0123456789 8080")
		if optOut {
			if !strings.Contains(string(log), "key=sk-live-0123456789") || got != "out sk-live-0123456789 8080" {
				t.Fatalf("opt-out still redacts: log %q, redact %q", log, got)
			}
			continue
		}
		if !strings.Contains(string(log), "key=[redacted:API_KEY] port=8080") {
			t.Fatalf("resume.log %q", log)
		}
		if got != "out [redacted:API_KEY] 8080" {
			t.Fatalf("Redact %q", got)
		}
	}
}
