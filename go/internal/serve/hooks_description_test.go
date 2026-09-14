package serve

import (
	"net/http"
	"os"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func TestHookDescriptionsCurrentAndHistorical(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	path := seedHook(t, f.home, "post-result", "guard.js", "// Description: current\nthrow new Error('listing must not execute');")
	now := time.Now()
	f.seed(t, "01a00000-0000-7000-8000-00000000desc",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "hook", Data: map[string]any{
			"event": "post-result", "name": "guard.js", "path": path, "description": "historical", "error": "failed then",
		}},
		history.Entry{Seq: 3, At: now.Add(time.Second), Kind: "hook", Data: map[string]any{
			"event": "stop", "name": "legacy.js", "path": path,
		}},
		history.Entry{Seq: 4, At: now.Add(2 * time.Second), Kind: "hook", Data: map[string]any{
			"event": "post-result", "name": "rules", "description": "Go hook purpose",
		}},
	)
	for _, current := range []string{"current", "edited", ""} {
		if current == "edited" {
			seedHook(t, f.home, "post-result", "guard.js", "// Description: edited\nreturn {};")
		} else if current == "" {
			if err := os.Remove(path); err != nil {
				t.Fatal(err)
			}
		}
		code, body := f.do(t, "GET", "/api/hooks", "")
		if code != http.StatusOK {
			t.Fatalf("GET = %d %v", code, body)
		}
		rows := body["hooks"].([]any)
		if current == "" {
			if len(rows) != 0 {
				t.Fatalf("deleted file still installed: %v", rows)
			}
		} else {
			if len(rows) != 1 {
				t.Fatalf("installed = %v", rows)
			}
			row := rows[0].(map[string]any)
			if row["description"] != current || row["failing"] != true {
				t.Fatalf("installed current metadata/status = %v", row)
			}
		}
		fires := body["fires"].([]any)
		if len(fires) != 3 {
			t.Fatalf("fires = %v", fires)
		}
		if got := fires[0].(map[string]any)["description"]; got != "Go hook purpose" {
			t.Fatalf("Go description = %v", got)
		}
		if _, present := fires[1].(map[string]any)["description"]; present {
			t.Fatalf("legacy acquired metadata: %v", fires[1])
		}
		if got := fires[2].(map[string]any)["description"]; got != "historical" {
			t.Fatalf("historical description = %v", got)
		}
	}
}

func TestInstalledDescriptionAbsentAfterCode(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	seedHook(t, f.home, "stop", "legacy.js", "return {};\n// Description: not metadata")
	code, body := f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("GET = %d %v", code, body)
	}
	rows := body["hooks"].([]any)
	if len(rows) != 1 {
		t.Fatalf("installed = %v", rows)
	}
	if _, present := rows[0].(map[string]any)["description"]; present {
		t.Fatalf("non-header metadata = %v", rows[0])
	}
}
