package engine

import (
	"bytes"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/plugins/history"
)

// `bough engine` on a real session: inspect prints the store, script
// turns it into a tape llm-script loads, and reproject restores the
// rows a crash lost between the store and the history file.
func TestEngineCLI(t *testing.T) {
	t.Parallel()
	r := smoke(t)
	id := strings.TrimSuffix(filepath.Base(r.path), ".jsonl")

	var out bytes.Buffer
	if err := runCLI(&out, r.store, filepath.Dir(r.path), []string{"inspect", id}); err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"input", "turn", "model_response", "tool_call_status", "bash(", "text ran: hi from codemode"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("inspect lacks %q:\n%s", want, out.String())
		}
	}

	tape := filepath.Join(t.TempDir(), "tape.json")
	if err := runCLI(&out, r.store, filepath.Dir(r.path), []string{"script", "-o", tape, id}); err != nil {
		t.Fatal(err)
	}
	steps, err := fake.Load(tape)
	if err != nil {
		t.Fatalf("the tape does not load: %v", err)
	}
	if len(steps) != 2 || len(steps[0].Output) != 1 {
		t.Fatalf("tape steps = %+v", steps)
	}

	// Nothing is missing from the live file.
	out.Reset()
	if err := runCLI(&out, r.store, filepath.Dir(r.path), []string{"reproject", "--dry-run", r.path}); err != nil {
		t.Fatal(err)
	}
	if out.Len() != 0 {
		t.Fatalf("dry run on a complete file printed:\n%s", out.String())
	}
	// A copy that lost every projected row gets them back.
	es, err := history.ReadFile(r.path)
	if err != nil {
		t.Fatal(err)
	}
	crashed := filepath.Join(t.TempDir(), id+".jsonl")
	h, err := history.Open(crashed)
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range es {
		if _, projected := e.Data["hseq"]; !projected {
			h.Append(e.Kind, e.Data)
		}
	}
	if err := h.Close(); err != nil {
		t.Fatal(err)
	}
	if err := runCLI(&out, r.store, filepath.Dir(r.path), []string{"reproject", crashed}); err != nil {
		t.Fatal(err)
	}
	back, err := history.ReadFile(crashed)
	if err != nil {
		t.Fatal(err)
	}
	if got := kinds(back); !slices.Contains(got, "call") || !slices.Contains(got, "assistant") {
		t.Fatalf("reprojected file: %v", got)
	}
}
