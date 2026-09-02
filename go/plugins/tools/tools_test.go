package tools

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

func TestToolsViaCodemode(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, err := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}

	out, err := cm.Run(`tools.bash("echo hi")`)
	if err != nil {
		t.Fatalf("bash: %v", err)
	}
	if !strings.Contains(out, "hi") {
		t.Errorf("bash output: %q", out)
	}

	path := filepath.Join(t.TempDir(), "f.txt")
	code := `tools.patch(` + jsStr(path) + `, "", "abc\ndef\n"); tools.view(` + jsStr(path) + `)`
	out, err = cm.Run(code)
	if err != nil {
		t.Fatalf("patch/view: %v", err)
	}
	if out != "1│abc\n2│def\n" {
		t.Errorf("view: %q", out)
	}
}

func TestBashTimeoutMessage(t *testing.T) {
	saved := bashTimeout
	bashTimeout = 200 * time.Millisecond
	t.Cleanup(func() { bashTimeout = saved })
	st := &Stats{}
	_, err := st.bash("sleep 5")
	if err == nil {
		t.Fatal("want error")
	}
	if !strings.HasPrefix(err.Error(), "bash: killed after 200ms: sleep 5") {
		t.Errorf("timeout message = %q", err.Error())
	}
	if _, exit, ran := st.Take(); !ran || exit != -1 {
		t.Errorf("exit = %d ran = %v, want -1 true", exit, ran)
	}
}

func TestTurnStats(t *testing.T) {
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	cm, _ := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	st, err := kernel.Get[*Stats](ctx, "turn-stats")
	if err != nil {
		t.Fatal(err)
	}
	if files, _, ran := st.Take(); len(files) != 0 || ran {
		t.Fatalf("fresh stats = %v %v", files, ran)
	}
	path := filepath.Join(t.TempDir(), "f.txt")
	if _, err := cm.Run(`tools.patch(` + jsStr(path) + `, "", "x"); try { tools.bash("exit 3") } catch (e) {}`); err != nil {
		t.Fatal(err)
	}
	files, exit, ran := st.Take()
	if len(files) != 1 || files[0] != path || exit != 3 || !ran {
		t.Errorf("Take = %v %d %v", files, exit, ran)
	}
	if files, _, ran := st.Take(); len(files) != 0 || ran {
		t.Errorf("Take did not reset: %v %v", files, ran)
	}
}

func jsStr(s string) string {
	return `"` + strings.ReplaceAll(s, `"`, `\"`) + `"`
}

// view numbers lines (width from the last number) and honors a range;
// patch replaces exactly one occurrence, refuses zero or many, and
// creates a file only with an empty old.
func TestViewAndPatch(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "a.txt")
	st := &Stats{}
	if _, err := st.patch(path, "x", "y"); err == nil {
		t.Fatal("patch on a missing file with non-empty old should fail")
	}
	if out, err := st.patch(path, "", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n"); err != nil || !strings.HasPrefix(out, "created") {
		t.Fatalf("create = (%q, %v)", out, err)
	}
	if _, err := st.patch(path, "", "again"); err == nil {
		t.Fatal("empty old on an existing file must not overwrite it")
	}
	got, err := view(path, 9, 10)
	if err != nil || got != " 9│nine\n10│ten\n" {
		t.Fatalf("view range = (%q, %v)", got, err)
	}
	if _, err := view(path, 99); err == nil {
		t.Fatal("start past the end should error")
	}
	if _, err := st.patch(path, "zzz", "q"); err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatalf("missing old = %v", err)
	}
	if _, err := st.patch(path, "e", "E"); err == nil || !strings.Contains(err.Error(), "occurs") {
		t.Fatalf("ambiguous old = %v", err)
	}
	if out, err := st.patch(path, "two\n", "2\n2b\n"); err != nil || out != "patched "+path+" (+1 lines)" {
		t.Fatalf("patch = (%q, %v)", out, err)
	}
	if got, _ := view(path, 1, 3); got != "1│one\n2│2\n3│2b\n" {
		t.Fatalf("after patch: %q", got)
	}
	files, _, _ := st.Take()
	if len(files) != 2 {
		t.Fatalf("stats files = %v", files)
	}
}

// Cancelling the run's context kills the command AND its children
// (the whole process group) at once: a cancelled turn must not wait
// out a sleep, and must not leave the sleep running.
func TestBashDiesWithTheRunContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	st := &Stats{runCtx: func() context.Context { return ctx }}
	marker := fmt.Sprintf("bough-cancel-test-%d", os.Getpid())
	done := make(chan error, 1)
	start := time.Now()
	go func() {
		_, err := st.bash("sleep 30; echo " + marker + "; sleep 30")
		done <- err
	}()
	time.Sleep(300 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err == nil || !strings.HasPrefix(err.Error(), "bash: cancelled: ") {
			t.Fatalf("want a cancelled error, got %v", err)
		}
		if time.Since(start) > 5*time.Second {
			t.Fatalf("cancel took %s: waited for the command", time.Since(start))
		}
	case <-time.After(10 * time.Second):
		t.Fatal("bash did not return after cancel")
	}
	time.Sleep(200 * time.Millisecond)
	out, _ := exec.Command("pgrep", "-f", "sleep 30; echo "+marker).Output()
	if strings.TrimSpace(string(out)) != "" {
		t.Fatalf("the shell survived the cancel: pids %s", out)
	}
}
