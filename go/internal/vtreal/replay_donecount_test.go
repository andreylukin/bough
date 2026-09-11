package vtreal

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

// doneCount counts the newest session only: an older session under the
// same $HOME must not make a wait for this run's turns return early.
func TestDoneCountNewestSessionOnly(t *testing.T) {
	t.Parallel()
	a := &app{t: t, home: t.TempDir()}
	dir := filepath.Join(a.home, ".bough", "history")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	old := filepath.Join(dir, "old.jsonl")
	body := `{"seq":1,"kind":"done","data":{}}` + "\n" + `{"seq":2,"kind":"cancelled","data":{}}` + "\n"
	if err := os.WriteFile(old, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	past := time.Now().Add(-time.Hour)
	if err := os.Chtimes(old, past, past); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "new.jsonl"), []byte(`{"seq":1,"kind":"meta","data":{}}`+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if n := a.doneCount(); n != 0 {
		t.Fatalf("doneCount = %d, want 0 (the old session's turns are not this run's)", n)
	}
	if a.waitDone(1, 200*time.Millisecond) {
		t.Fatal("waitDone(1) returned on the old session's dones")
	}
}
