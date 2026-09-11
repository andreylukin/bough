package graph

import (
	"database/sql"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/dop251/goja"
)

// graphArtifactsMemoryVM binds tools.graph the way Apply does.
func graphArtifactsMemoryVM(t *testing.T, svc *Service) *goja.Runtime {
	t.Helper()
	vm := goja.New()
	tools := vm.NewObject()
	_ = tools.Set("graph", svc.jsObject(vm))
	_ = vm.Set("tools", tools)
	return vm
}

// Empty and wrong-typed arguments to every verb throw a JS error the
// model can catch; none panics the host.
func TestGraphArtifactsMemoryEmptyArgs(t *testing.T) {
	st := openTemp(t)
	ep, _ := st.Episode("session", "t")
	vm := graphArtifactsMemoryVM(t, &Service{Store: st, episode: ep})
	for _, js := range []string{
		`tools.graph.resolve()`, `tools.graph.resolve("")`,
		`tools.graph.neighbors()`, `tools.graph.timeline("")`,
		`tools.graph.assert()`, `tools.graph.assert("", "", "", "")`,
		`tools.graph.assert("a b", "relates", "/", "x")`,
	} {
		v, err := vm.RunString(`try { ` + js + `; "ok" } catch (e) { "caught: " + e.message }`)
		if err != nil {
			t.Errorf("%s: uncaught %v", js, err)
			continue
		}
		if !strings.HasPrefix(v.String(), "caught: ") {
			t.Errorf("%s: no error: %s", js, v)
		}
	}
	for _, js := range []string{`tools.graph.search()`, `tools.graph.search("", -1)`, `tools.graph.world()`, `tools.graph.invalidate(0, "")`} {
		if _, err := vm.RunString(`try { ` + js + ` } catch (e) { e.message }`); err != nil {
			t.Errorf("%s: uncaught %v", js, err)
		}
	}
	vm2 := graphArtifactsMemoryVM(t, &Service{Store: st})
	v, _ := vm2.RunString(`try { tools.graph.assert("repo:a", "relates", "repo:b", "e") } catch (e) { e.message }`)
	if !strings.Contains(v.String(), "no session episode") {
		t.Fatalf("no episode: %s", v)
	}
}

// A closed store (the row unmounted under a running block) throws,
// it does not panic.
func TestGraphArtifactsMemoryClosedStore(t *testing.T) {
	st, err := Open(filepath.Join(t.TempDir(), "g.db"))
	if err != nil {
		t.Fatal(err)
	}
	ep, _ := st.Episode("session", "t")
	st.Close()
	vm := graphArtifactsMemoryVM(t, &Service{Store: st, episode: ep})
	for _, js := range []string{`tools.graph.search("x", 5)`, `tools.graph.assert("repo:a", "relates", "repo:b", "e")`, `tools.graph.world()`} {
		v, err := vm.RunString(`try { ` + js + `; "ok" } catch (e) { "caught: " + e.message }`)
		if err != nil || !strings.HasPrefix(v.String(), "caught: ") {
			t.Errorf("%s on a closed store: %v %v", js, v, err)
		}
	}
}

// A db held by another writer: assert errors within the busy timeout
// instead of hanging, and works once the lock clears. Slow (~5 s).
func TestGraphArtifactsMemoryLockedDB(t *testing.T) {
	if os.Getenv("BOUGH_SOAK_TOOLS_ARTIFACTS_MEMORY") != "1" {
		t.Skip("slow: set BOUGH_SOAK_TOOLS_ARTIFACTS_MEMORY=1")
	}
	path := filepath.Join(t.TempDir(), "g.db")
	st, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	ep, _ := st.Episode("session", "t")
	other, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatal(err)
	}
	defer other.Close()
	conn, err := other.Conn(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if _, err := conn.ExecContext(t.Context(), "BEGIN EXCLUSIVE"); err != nil {
		t.Fatal(err)
	}
	svc := &Service{Store: st, episode: ep}
	start := time.Now()
	if _, err := svc.AssertAs("cheap", "repo:a", "relates", "memory:x", "e"); err == nil {
		t.Fatal("assert through an exclusive lock succeeded")
	}
	if d := time.Since(start); d > 15*time.Second {
		t.Fatalf("assert hung %s", d)
	}
	_, _ = conn.ExecContext(t.Context(), "ROLLBACK")
	if _, err := svc.AssertAs("cheap", "repo:a", "relates", "memory:x", "e"); err != nil {
		t.Fatalf("store not usable after the lock cleared: %v", err)
	}
}
