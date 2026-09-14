package hooks

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// The fixture changes HOME and cwd, so these fire tests cannot run in parallel.
func TestDiskDescriptionCapturedBeforeExecution(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	const event = "post-result"
	writeHook(t, cwd, event, "a.js", "// Description: original\nreturn {};")
	writeHook(t, cwd, event, "b.js", "// Description: broken\nthrow new Error('broken');")
	if _, err := s.Fire(context.Background(), event, map[string]any{}); err == nil {
		t.Fatal("want execution failure")
	}
	writeHook(t, cwd, event, "a.js", "// Description: edited\nreturn {};")
	if err := os.Remove(filepath.Join(cwd, ".bough", "hooks", event, "b.js")); err != nil {
		t.Fatal(err)
	}
	records := s.TakeFireRecords()
	if len(records) != 2 || records[0]["description"] != "original" || records[1]["description"] != "broken" || records[1]["error"] == "" {
		t.Fatalf("historical records = %#v", records)
	}
	if _, err := s.Fire(context.Background(), event, map[string]any{}); err != nil {
		t.Fatal(err)
	}
	fires := s.Fires(0)
	if len(fires) != 3 || fires[0].Description != "edited" || fires[1].Description != "broken" || fires[2].Description != "original" {
		t.Fatalf("fires = %#v", fires)
	}
}

func TestGoDescriptionAndLegacyAdd(t *testing.T) {
	s := fixture(t)
	fn := func(map[string]any) map[string]any { return nil }
	remove := s.AddWithDescription("stop", "described", "Declared purpose.", fn)
	s.Add("stop", "legacy", fn)
	if _, err := s.Fire(context.Background(), "stop", map[string]any{}); err != nil {
		t.Fatal(err)
	}
	records := s.TakeFireRecords()
	if len(records) != 2 || records[0]["description"] != "Declared purpose." {
		t.Fatalf("records = %#v", records)
	}
	if _, ok := records[1]["description"]; ok {
		t.Fatalf("legacy description should be absent: %#v", records[1])
	}
	fires := s.Fires(0)
	for _, f := range fires {
		raw, err := json.Marshal(f)
		if err != nil {
			t.Fatal(err)
		}
		var got map[string]any
		if err := json.Unmarshal(raw, &got); err != nil {
			t.Fatal(err)
		}
		_, present := got["description"]
		if present != (f.Name == "described") {
			t.Fatalf("description JSON = %s", raw)
		}
	}
	remove()
	if _, err := s.Fire(context.Background(), "stop", map[string]any{}); err != nil {
		t.Fatal(err)
	}
	if got := s.TakeFires(); len(got) != 1 || got[0].Name != "legacy" {
		t.Fatalf("fires after removal = %#v", got)
	}
}
