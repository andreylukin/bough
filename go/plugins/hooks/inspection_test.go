package hooks

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

func TestInspectionGoSnapshots(t *testing.T) {
	// fixture changes HOME and cwd, so these integration tests cannot run parallel.
	s := fixture(t)
	nested := map[string]any{"value": "before"}
	payload := map[string]any{"input": "original", "nested": nested}
	first := map[string]any{"input": "rewritten", "nested": nested, "notice": "first"}
	s.Add("inspect", "first", func(p map[string]any) map[string]any {
		p["nested"].(map[string]any)["value"] = "during"
		return first
	})
	s.Add("inspect", "second", func(p map[string]any) map[string]any {
		if p["input"] != "rewritten" {
			t.Fatalf("actual chained input: %v", p)
		}
		nested["value"] = "later"
		return nil
	})
	res, err := s.Fire(context.Background(), "inspect", payload)
	if err != nil {
		t.Fatal(err)
	}
	delete(res, "notice")
	first["input"] = "after"
	payload["input"] = "after"
	fires := s.TakeFires()
	if len(fires) != 2 {
		t.Fatalf("fires: %v", fires)
	}
	if string(fires[0].Input) != `{"input":"original","nested":{"value":"before"}}` {
		t.Fatalf("input was mutated: %s", fires[0].Input)
	}
	if string(fires[0].Output) != `{"input":"rewritten","nested":{"value":"during"},"notice":"first"}` {
		t.Fatalf("output was mutated: %s", fires[0].Output)
	}
	if string(fires[1].Input) != `{"input":"rewritten","nested":{"value":"during"}}` || string(fires[1].Output) != "null" {
		t.Fatalf("second: %+v", fires[1])
	}
	if fires[0].Path != "" {
		t.Fatal("Go hook has a disk definition")
	}
	fires[0].Input[0] = '!'
	newest := s.Fires(0)
	newest[0].Output[0] = '!'
	if !json.Valid(s.Fires(0)[1].Input) || string(s.Fires(0)[0].Output) != "null" {
		t.Fatal("ledger aliases returned snapshots")
	}
}

func TestInspectionJSActualInputsAndDefinition(t *testing.T) {
	s := fixture(t)
	home, _ := os.UserHomeDir()
	cwd, _ := os.Getwd()
	writeHook(t, home, "inspect", "a.js", `return {wrong: true}`)
	writeHook(t, cwd, "inspect", "a.js", `event.nested.value = "mutated"; return {input: "js rewrite", notice: "one"}`)
	writeHook(t, cwd, "inspect", "b.js", `return {seen: event.input, notice: "two"}`)
	writeHook(t, cwd, "inspect", "c.js", `return null`)
	s.Add("inspect", "go", func(map[string]any) map[string]any { return map[string]any{"input": "go rewrite"} })
	_, err := s.Fire(context.Background(), "inspect", map[string]any{"input": "original", "nested": map[string]any{"value": "before"}})
	if err != nil {
		t.Fatal(err)
	}
	records := s.TakeFireRecords()
	if len(records) != 4 {
		t.Fatalf("records: %v", records)
	}
	if records[1]["path"] != filepath.Join(cwd, ".bough", "hooks", "inspect", "a.js") {
		t.Fatalf("definition: %v", records[1])
	}
	var in, out map[string]any
	if err := json.Unmarshal(records[1]["input"].(json.RawMessage), &in); err != nil {
		t.Fatal(err)
	}
	if in["input"] != "go rewrite" || in["nested"].(map[string]any)["value"] != "before" {
		t.Fatalf("input: %v", in)
	}
	if err := json.Unmarshal(records[2]["output"].(json.RawMessage), &out); err != nil {
		t.Fatal(err)
	}
	// JS results are merged, not fed to later JS hooks. Inspection must not
	// silently change that existing behavior.
	if out["seen"] != "go rewrite" || out["notice"] != "two" {
		t.Fatalf("output: %v", out)
	}
	if string(records[3]["output"].(json.RawMessage)) != "null" {
		t.Fatalf("null: %v", records[3])
	}
	records[1]["input"].(json.RawMessage)[0] = '!'
	if !json.Valid(s.Fires(0)[2].Input) {
		t.Fatal("history map aliases ring")
	}
	if s.TakeFireRecords() != nil {
		t.Fatal("records did not drain")
	}
}

func TestInspectionFailureAndEffectiveOutput(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "inspect", "a.js", `throw new Error("failed")`)
	writeHook(t, cwd, "inspect", "b.js", `return {result: "x".repeat(12000)}`)
	res, err := s.Fire(context.Background(), "inspect", map[string]any{"result": "before"})
	if err == nil || !strings.Contains(err.Error(), "failed") {
		t.Fatalf("failure: %v", err)
	}
	fires := s.TakeFires()
	if fires[0].Error == "" || string(fires[0].Output) != "null" || string(fires[0].Input) != `{"result":"before"}` {
		t.Fatalf("failed snapshot: %+v", fires[0])
	}
	var output map[string]any
	if err := json.Unmarshal(fires[1].Output, &output); err != nil {
		t.Fatal(err)
	}
	if output["result"] != res["result"] || len(fires[1].Truncated) != 1 {
		t.Fatal("snapshot is not the capped effective result")
	}
}

func TestInspectionLimitsAndSerializationFailure(t *testing.T) {
	t.Parallel()
	huge := strings.Repeat("x", maxInspectionBytes)
	b, n, cut, failure := snapshot(map[string]any{"huge": huge})
	if b != nil || n <= maxInspectionBytes || !cut || failure != "" {
		t.Fatalf("limit: %d %v %q", n, cut, failure)
	}
	cyclic := map[string]any{}
	cyclic["self"] = cyclic
	for _, v := range []any{cyclic, func() {}} {
		b, _, cut, failure := snapshot(v)
		if b != nil || cut || failure == "" {
			t.Fatalf("unserializable: %s %v %s", b, cut, failure)
		}
	}
}

func TestInspectionCaptureDoesNotChangeGoResult(t *testing.T) {
	s := fixture(t)
	result := map[string]any{"large": strings.Repeat("x", maxInspectionBytes), "keep": true}
	s.Add("inspect", "large", func(map[string]any) map[string]any { return result })
	got, err := s.Fire(context.Background(), "inspect", map[string]any{"unsupported": func() {}})
	if err != nil || got["large"] != result["large"] {
		t.Fatalf("behavior changed: %v", err)
	}
	f := s.TakeFireRecords()[0]
	if f["inputError"] == "" || f["outputTruncated"] != true || f["outputBytes"].(int) <= maxInspectionBytes {
		t.Fatalf("metadata: %v", f)
	}
	if _, ok := f["output"]; ok {
		t.Fatal("oversize output retained")
	}
}

func TestInspectionConcurrentLedger(t *testing.T) {
	t.Parallel()
	s := &Service{}
	var wg sync.WaitGroup
	for range 4 {
		wg.Go(func() {
			for range 100 {
				f := Fire{}
				f.captureInput(map[string]any{"n": 1})
				f.captureOutput(nil)
				s.record(f)
				s.Fires(2)
				s.TakeFireRecords()
			}
		})
	}
	wg.Wait()
}
