package serve

import (
	"net/http"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// Every child serve starts is the person's: its env says BOUGH_ORIGIN=web,
// which the child records on its meta entry.
func TestSupervisorChildrenAreWebOrigin(t *testing.T) {
	t.Parallel()
	f := newFixture(t, envNewID+"=sess-origin")
	id, err := f.sup.Create(CreateOptions{Cwd: f.home})
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	es, err := history.Read(filepath.Join(f.hist, id+".jsonl"))
	if err != nil || len(es) == 0 {
		t.Fatalf("read = %v, %v", es, err)
	}
	if es[0].Data["origin"] != "web" {
		t.Fatalf("meta = %+v, want origin web", es[0].Data)
	}
}

func TestRowMarksBackground(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now()
	f.seed(t, "01a00000-0000-7000-8000-00000000bg01",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home, "origin": "headless"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "/llm-wiki ingest x"}},
	)
	f.seed(t, "01a00000-0000-7000-8000-00000000me01",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home, "origin": "web"}},
	)
	for id, want := range map[string]any{"01a00000-0000-7000-8000-00000000bg01": true, "01a00000-0000-7000-8000-00000000me01": nil} {
		code, body := f.do(t, "GET", "/api/sessions/"+id, "")
		if code != http.StatusOK {
			t.Fatalf("GET %s = %d", id, code)
		}
		sess, _ := body["session"].(map[string]any)
		if sess["background"] != want {
			t.Errorf("%s: background = %v, want %v", id, sess["background"], want)
		}
	}
}
