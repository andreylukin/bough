package kernel

import (
	"io"
	"os"
	"strings"
	"testing"
)

// captureStderr runs fn with os.Stderr redirected and returns what it wrote.
func captureStderr(t *testing.T, fn func()) string {
	t.Helper()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	old := os.Stderr
	os.Stderr = w
	fn()
	os.Stderr = old
	w.Close()
	b, _ := io.ReadAll(r)
	return string(b)
}

func TestLogfGatedByVerbose(t *testing.T) {
	old := Verbose
	defer func() { Verbose = old }()

	Verbose = false
	if out := captureStderr(t, func() { Logf("kernel: quiet %d\n", 1) }); out != "" {
		t.Fatalf("Verbose=false printed %q", out)
	}
	Verbose = true
	if out := captureStderr(t, func() { Logf("kernel: loud %d\n", 2) }); !strings.Contains(out, "kernel: loud 2") {
		t.Fatalf("Verbose=true printed %q", out)
	}
}

// A row reload triggered by a changed service is chatter, not a
// warning: silent unless Verbose.
func TestReloadLineIsVerboseOnly(t *testing.T) {
	old := Verbose
	defer func() { Verbose = old }()
	Verbose = false

	var log []string
	Register("log-a", func() Plugin {
		return &testPlugin{name: "log-a", provide: []string{"log-svc"}, applied: &log}
	})
	Register("log-b", func() Plugin {
		return &testPlugin{name: "log-b", inject: []string{"log-svc"}, applied: &log}
	})
	c := NewContext()
	rows := []Row{
		{ID: "a", Plugin: "log-a", Config: map[string]any{"v": "1"}},
		{ID: "b", Plugin: "log-b"},
	}
	if err := c.Mount(rows); err != nil {
		t.Fatal(err)
	}
	rows[0].Config = map[string]any{"v": "2"}
	out := captureStderr(t, func() {
		if err := c.Reconcile(rows); err != nil {
			t.Error(err)
		}
	})
	if strings.Contains(out, "reloading") {
		t.Fatalf("reload line printed without --verbose:\n%s", out)
	}
	c.Unmount()
}
