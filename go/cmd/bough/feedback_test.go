//go:build !windows

package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestFeedbackDraftUsesOnlyNamedMetadata(t *testing.T) {
	t.Parallel()
	draft := feedbackDraft("v1.2.3", "linux", "arm64", "llm=llm-echo, loop=loop")
	for _, want := range []string{"v1.2.3", "linux/arm64", "llm=llm-echo, loop=loop", feedbackProblem} {
		if !strings.Contains(draft, want) {
			t.Fatalf("draft missing %q", want)
		}
	}
	if _, _, err := parseFeedback(draft); err == nil {
		t.Fatal("unedited draft was accepted")
	}
}

func TestFeedbackPostsReviewedBody(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	editor := filepath.Join(dir, "editor")
	gh := filepath.Join(dir, "gh")
	if err := os.WriteFile(editor, []byte("#!/bin/sh\ncat > \"$1\" <<'EOF'\n# A crash after resume\n\n## What happened\nThe resumed session exited.\n\n## Environment\n- Bough: v1.2.3\nEOF\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(gh, []byte("#!/bin/sh\nif [ \"$2\" = view ]; then\n  printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/opened\"\n  exit 0\nfi\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\ncp \"$8\" \"$(dirname \"$0\")/body\"\nprintf 'https://github.com/andreylukin/bough/issues/123\\n'\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	var out, errOut bytes.Buffer
	input := filepath.Join(dir, "input")
	if err := os.WriteFile(input, []byte("yes\nyes\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	in, err := os.Open(input)
	if err != nil {
		t.Fatal(err)
	}
	defer in.Close()
	if err := submitFeedback(in, &out, &errOut, editor, gh, feedbackDraft("v1.2.3", "linux", "amd64", "llm=llm-echo")); err != nil {
		t.Fatal(err)
	}
	args, err := os.ReadFile(filepath.Join(dir, "args"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(args), "issue\ncreate\n--repo\n"+feedbackRepo+"\n--title\nA crash after resume\n--body-file\n") {
		t.Fatalf("gh args = %q", args)
	}
	body, err := os.ReadFile(filepath.Join(dir, "body"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(body), "# A crash after resume") || !strings.Contains(string(body), "The resumed session exited.") {
		t.Fatalf("posted body = %q", body)
	}
	if !strings.Contains(out.String(), "Issue created: https://github.com/andreylukin/bough/issues/123") {
		t.Fatalf("output = %q", out.String())
	}
	opened, err := os.ReadFile(filepath.Join(dir, "opened"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(opened), "issue\nview\nhttps://github.com/andreylukin/bough/issues/123\n--web\n") {
		t.Fatalf("browser args = %q", opened)
	}
}

func TestFeedbackCancelDoesNotPost(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	editor := filepath.Join(dir, "editor")
	gh := filepath.Join(dir, "gh")
	if err := os.WriteFile(editor, []byte("#!/bin/sh\ncat > \"$1\" <<'EOF'\n# A crash\n\n## What happened\nIt crashed.\nEOF\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(gh, []byte("#!/bin/sh\ntouch \"$(dirname \"$0\")/called\"\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	var out, errOut bytes.Buffer
	input := filepath.Join(dir, "input")
	if err := os.WriteFile(input, []byte("no\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	in, err := os.Open(input)
	if err != nil {
		t.Fatal(err)
	}
	defer in.Close()
	if err := submitFeedback(in, &out, &errOut, editor, gh, feedbackDraft("v1", "linux", "amd64", "loop=loop")); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "called")); !os.IsNotExist(err) {
		t.Fatalf("gh ran after cancellation: %v", err)
	}
}
