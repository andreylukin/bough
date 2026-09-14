package serve

import (
	"encoding/json"
	"net/http"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func TestHookInspectionHistoryAPI(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	now := time.Now()
	path := filepath.Join(f.home, ".bough", "hooks", "post-result", "inspect.js")
	f.seed(t, "01a00000-0000-7000-8000-00000000fire",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "hook", Data: map[string]any{
			"event": "post-result", "name": "inspect.js", "path": path,
			"input": json.RawMessage(`{"result":"before","nested":[1,true]}`), "output": json.RawMessage("null"),
			"inputBytes": 37, "outputBytes": 4,
		}},
		history.Entry{Seq: 3, At: now.Add(time.Second), Kind: "hook", Data: map[string]any{
			"event": "post-result", "name": "large.js",
			"inputTruncated": true, "inputBytes": 70000,
			"outputError": "value is not JSON serializable",
		}},
		history.Entry{Seq: 4, At: now.Add(2 * time.Second), Kind: "hook", Data: map[string]any{
			"event": "post-result", "name": "legacy.js",
		}},
	)
	code, body := f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("status: %d %v", code, body)
	}
	fires := body["fires"].([]any)
	if len(fires) != 3 {
		t.Fatalf("fires: %v", fires)
	}
	legacy := fires[0].(map[string]any)
	if _, ok := legacy["input"]; ok {
		t.Fatal("legacy input invented")
	}
	if _, ok := legacy["output"]; ok {
		t.Fatal("legacy output invented")
	}
	large := fires[1].(map[string]any)
	if large["inputTruncated"] != true || large["inputBytes"] != float64(70000) || large["outputError"] != "value is not JSON serializable" {
		t.Fatalf("capture metadata lost: %v", large)
	}
	if _, ok := large["input"]; ok {
		t.Fatal("truncated input invented")
	}
	got := fires[2].(map[string]any)
	if output, ok := got["output"]; !ok || output != nil {
		t.Fatalf("captured null lost: %v", got)
	}
	if got["path"] != path || got["outputBytes"] != float64(4) || got["input"].(map[string]any)["result"] != "before" {
		t.Fatalf("inspection lost: %v", got)
	}
}

func TestHookInspectionDefinitionIdentity(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	home := filepath.Join(root, "home", "same.js")
	project := filepath.Join(root, "project", "same.js")
	other := filepath.Join(root, "other-project", "same.js")
	rows := []HookRow{
		{Name: "same.js", Event: "stop", Path: home, Shadowed: true},
		{Name: "same.js", Event: "stop", Path: project},
	}
	now := time.Now()
	fires := []HookFire{
		{At: now, Name: "same.js", Event: "stop", Path: other, Error: "other project"},
		{At: now, Name: "same.js", Event: "stop", Input: json.RawMessage("{}"), Error: "Go hook"},
		{At: now, Name: "same.js", Event: "stop", Path: project, Decision: "rewrote"},
		{At: now.Add(-time.Second), Name: "same.js", Event: "stop", Path: home, Decision: "denied"},
	}
	got := withLast(rows, fires)
	if got[0].LastDecision != "denied" || got[1].LastDecision != "rewrote" || got[0].Failing || got[1].Failing {
		t.Fatalf("definitions mixed: %+v", got)
	}
	legacy := withLast([]HookRow{
		{Name: "same.js", Event: "stop", Path: home, Shadowed: true},
		{Name: "same.js", Event: "stop", Path: project},
	}, []HookFire{{At: now, Name: "same.js", Event: "stop"}})
	if legacy[0].LastFired != nil || legacy[1].LastFired == nil {
		t.Fatalf("legacy attribution: %+v", legacy)
	}
}
