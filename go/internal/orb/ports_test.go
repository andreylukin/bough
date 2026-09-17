package orb

import (
	"context"
	"errors"
	"fmt"
	"net"
	"slices"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

func freePort(t *testing.T) int {
	t.Helper()
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}

// Opted-in ports publish on 127.0.0.1; one already taken on the host is
// skipped with a reason instead of failing the orb, and the container IP
// is recorded for every surface.
func TestOpenPortsAndAddress(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	busy, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer busy.Close()
	taken, free := busy.Addr().(*net.TCPAddr).Port, freePort(t)
	p := newProject(t, home, "pt", "  - path: "+newRepo(t)+"\n")
	yml, _ := projectdef.ReadFile(home, "pt", projectdef.FileYAML)
	projectdef.WriteFile(home, "pt", projectdef.FileYAML, yml+fmt.Sprintf("ports: [%d, \"%d:80\"]\n", taken, free))
	p, _ = projectdef.Load(home, "pt")
	rt := container.NewFake()
	rt.Addr = "192.168.64.7"

	o, err := Open(ctx, rt, home, "s1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if got := rt.LastRun.Ports; !slices.Equal(got, []container.PortMap{{Host: free, Guest: 80}}) {
		t.Fatalf("published %v", got)
	}
	st, _ := ReadState(home, "s1")
	if st.IP != "192.168.64.7" || len(st.Ports) != 2 {
		t.Fatalf("state ip %q ports %+v", st.IP, st.Ports)
	}
	if st.Ports[0].Host != taken || !strings.Contains(st.Ports[0].Error, "in use") || st.Ports[1].Error != "" || st.Ports[1].Guest != 80 {
		t.Fatalf("ports %+v", st.Ports)
	}
	if line := PhaseLine(st, st.UpdatedAt); !strings.Contains(line, "192.168.64.7") {
		t.Fatalf("line %q", line)
	}

	// A reopen reuses the container: its forwards are the ones it was
	// created with, not a fresh check (its own port is "in use" by it).
	o.Stop(ctx)
	o2, err := Open(ctx, rt, home, "s1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if st2 := o2.State(); !slices.Equal(st2.Ports, st.Ports) {
		t.Fatalf("reopen ports %+v, want %+v", st2.Ports, st.Ports)
	}
}

// The runtime refusing a forward (a port taken since the check, or on
// restart of a stopped container) says which ports and what to change.
func TestStartPortCollisionMessage(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	port := freePort(t)
	newProject(t, home, "pc", "  - path: "+newRepo(t)+"\n")
	yml, _ := projectdef.ReadFile(home, "pc", projectdef.FileYAML)
	projectdef.WriteFile(home, "pc", projectdef.FileYAML, yml+fmt.Sprintf("ports: [%d]\n", port))
	p, _ := projectdef.Load(home, "pc")
	rt := container.NewFake()
	rt.FailStart = errors.New(`bind(descriptor:ptr:bytes:): Address already in use) (errno: 48)`)
	_, err := Open(ctx, rt, home, "s1", p, t.TempDir())
	if err == nil || !strings.Contains(err.Error(), fmt.Sprintf("127.0.0.1:%d", port)) || !strings.Contains(err.Error(), "ports:") {
		t.Fatalf("err %v", err)
	}
}
