package graph

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

// A graph file that cannot be written (nor its directory) still opens:
// the schema DDL must not run against it, and reads work.
func TestOpenReadonlyFile(t *testing.T) {
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("read-only via chmod is POSIX and not for root")
	}
	dir := filepath.Join(t.TempDir(), "graph")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	p := filepath.Join(dir, "graph.db")
	st, err := Open(p)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Upsert("person", "ann", "Ann", ""); err != nil {
		t.Fatal(err)
	}
	st.Close()
	os.Remove(p + "-wal")
	os.Remove(p + "-shm")
	if err := os.Chmod(p, 0o444); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0o555); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o755); _ = os.Chmod(p, 0o644) })

	st, err = Open(p)
	if err != nil {
		t.Fatalf("open read-only graph: %v", err)
	}
	defer st.Close()
	var n int
	if err := st.db.QueryRow(`SELECT count(*) FROM entities WHERE key='ann'`).Scan(&n); err != nil || n != 1 {
		t.Fatalf("read: n=%d err=%v", n, err)
	}
}
