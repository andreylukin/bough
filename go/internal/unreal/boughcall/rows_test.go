//go:build !windows

package boughcall

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/unreallabsai/unreal-agent/harness/operation"

	"github.com/andreylukin/bough/internal/agenttools"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/secrets"
	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/agenttools"
	"github.com/andreylukin/bough/plugins/ask"
	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/tools"
)

// These run the real tool rows behind a real LocalOperationManager: what
// the store would save is what the manager emits, so a leak here is a
// leak to disk and to the provider.

// rowsRig mounts the tool rows and points a rig at their registry.
func rowsRig(t *testing.T, o Options, setup func(*kernel.Context), rows ...kernel.Row) (*rig, *kernel.Context) {
	t.Helper()
	ctx := kernel.NewContext()
	if setup != nil {
		setup(ctx)
	}
	base := []kernel.Row{
		{ID: "agent-tools", Plugin: "agent-tools"},
		{ID: "codemode", Plugin: "codemode"},
		{ID: "tools", Plugin: "tools-basic"},
	}
	if err := ctx.Mount(append(base, rows...)); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		t.Fatal(err)
	}
	o.Tools = reg.Lookup
	r := newRig(t, o)
	r.reg = reg
	return r, ctx
}

type progressLog struct {
	mu  sync.Mutex
	log map[string]string
}

func (p *progressLog) add(call, text string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.log == nil {
		p.log = map[string]string{}
	}
	p.log[call] += text
}

func (p *progressLog) of(call string) string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.log[call]
}

// Native bash through the op: its output streams as progress for its
// call id while it runs, the row gets exit and cmd, and a non-zero exit
// is a failed op whose text leads with the exit.
func TestRowsBashThroughTheOp(t *testing.T) {
	t.Parallel()
	var p progressLog
	r, _ := rowsRig(t, Options{Progress: p.add}, nil)
	op := r.terminal(r.add("bash", "c1", `{"command":"printf 'one\\n'; sleep 0.2; printf 'two\\n'"}`, 0))
	text, h := render(op)
	if op.Status != operation.StatusCompleted || text != "one\ntwo\n" {
		t.Fatalf("bash = %s %q", op.Status, text)
	}
	if h.Detail != "printf 'one\\n'; sleep 0.2; printf 'two\\n'" || h.Data["exit"] != float64(0) {
		t.Fatalf("handle = %+v", h)
	}
	if got := p.of("c1"); got != "one\ntwo\n" {
		t.Fatalf("progress = %q", got)
	}
	op = r.terminal(r.add("bash", "c2", `{"command":"echo nope; exit 2"}`, 0))
	if text, h := render(op); op.Status != operation.StatusFailed || text != "Error: exit status 2\nnope\n" || h.Data["exit"] != float64(2) {
		t.Fatalf("failed bash = %s %q %+v", op.Status, text, h)
	}
}

// secretOrb is a project orb on the host whose project has one secret.
type secretOrb struct{ root string }

func (o secretOrb) Command(ctx context.Context, argv ...string) *exec.Cmd {
	return exec.CommandContext(ctx, argv[0], argv[1:]...)
}
func (o secretOrb) Root() string { return o.root }
func (o secretOrb) Redactor() *iorb.Redactor {
	return iorb.NewRedactor(map[string]string{"API_KEY": "sk-orb-0123456789"})
}

// A project session's secret printed by a command is redacted inside
// the tool, before the stream or any snapshot sees it, even split
// across writes; with no orb the call fails and nothing runs on the host.
func TestRowsProjectBashRedactsAndNeverFallsBack(t *testing.T) {
	t.Parallel()
	var p progressLog
	r, ctx := rowsRig(t, Options{Progress: p.add}, func(ctx *kernel.Context) {
		ctx.Provide("session-mode", "project")
	})
	marker := filepath.Join(t.TempDir(), "ran")
	op := r.terminal(r.add("bash", "c0", `{"command":"touch `+marker+`"}`, 0))
	if text, _ := render(op); op.Status != operation.StatusFailed || !strings.Contains(text, "orb not ready") {
		t.Fatalf("no orb = %s %q", op.Status, text)
	}
	if _, err := os.Stat(marker); err == nil {
		t.Fatal("a project call ran on the host with no orb")
	}
	ctx.Provide("orb", secretOrb{root: t.TempDir()})
	op = r.terminal(r.add("bash", "c1", `{"command":"for c in sk- orb -012 3456 789; do printf %s $c; sleep 0.05; done; echo"}`, 0))
	if text, _ := render(op); op.Status != operation.StatusCompleted || text != "[redacted:API_KEY]\n" {
		t.Fatalf("redacted bash = %s %q", op.Status, text)
	}
	if all := r.all(); strings.Contains(all, "sk-orb") {
		t.Fatalf("a snapshot holds the secret: %s", all)
	}
	if got := p.of("c1"); strings.Contains(got, "sk-orb") || !strings.Contains(got, "[redacted:API_KEY]") {
		t.Fatalf("progress = %q", got)
	}
}

// The secret tool's answer goes to the keychain and never into any op
// snapshot, which is what the store keeps and the provider reads.
// Not parallel: HOME and the keychain seam are process-wide.
func TestRowsSecretNeverReachesState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	dir := filepath.Join(home, ".bough", "projects", "demo")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "project.yml"), []byte("repos:\n  - path: "+home+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	stored := map[string]string{}
	saved := secrets.KeychainWrite
	secrets.KeychainWrite = func(service, value string) error { stored[service] = value; return nil }
	t.Cleanup(func() { secrets.KeychainWrite = saved })

	const value = "sk-typed-by-the-person-42"
	r, ctx := rowsRig(t, Options{}, nil, kernel.Row{ID: "ask", Plugin: "ask"})
	answers, err := kernel.Get[*ask.Asker](ctx, "ask-answers")
	if err != nil {
		t.Fatal(err)
	}
	ctx.On("loop/event", func(p any) {
		if ev, ok := p.(ask.Event); ok && ev.Secret {
			go answers.Answer(ev.ID, value)
		}
	})
	op := r.terminal(r.add("secret", "s1", `{"name":"API_KEY","question":"the API needs it","project":"demo"}`, 0))
	if text, _ := render(op); op.Status != operation.StatusCompleted || !strings.HasPrefix(text, "stored API_KEY as keychain:bough/demo/API_KEY") {
		t.Fatalf("secret = %s %q", op.Status, text)
	}
	if stored["bough/demo/API_KEY"] != value {
		t.Fatalf("keychain = %v", stored)
	}
	if all := r.all(); strings.Contains(all, value) {
		t.Fatalf("a snapshot holds the secret: %s", all)
	}
}
