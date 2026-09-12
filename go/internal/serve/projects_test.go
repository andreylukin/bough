package serve

import (
	"net/http"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

func seedSession(t *testing.T, f *apiFixture, id string) {
	t.Helper()
	now := time.Now()
	f.seed(t, id,
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/w"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "hi"}},
		history.Entry{Seq: 3, At: now, Kind: "done", Data: nil},
	)
}

func mkProject(t *testing.T, f *apiFixture, name string) string {
	t.Helper()
	code, body := f.do(t, "POST", "/api/projects", `{"name":"`+name+`"}`)
	if code != http.StatusOK {
		t.Fatalf("create %q = %d %v", name, code, body)
	}
	p, _ := body["project"].(map[string]any)
	id, _ := p["id"].(string)
	if id == "" {
		t.Fatalf("created project has no id: %v", body)
	}
	return id
}

func TestProjectsCreateListRename(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "Platform infra")

	_, body := f.do(t, "GET", "/api/projects", "")
	ps, _ := body["projects"].([]any)
	if len(ps) != 1 {
		t.Fatalf("projects = %v, want one", body)
	}
	if code, _ := f.do(t, "POST", "/api/projects/"+id+"/rename", `{"name":"Observability"}`); code != http.StatusOK {
		t.Fatalf("rename = %d", code)
	}
	_, body = f.do(t, "GET", "/api/projects", "")
	ps, _ = body["projects"].([]any)
	first, _ := ps[0].(map[string]any)
	if first["name"] != "Observability" {
		t.Errorf("after rename = %v", first)
	}
}

func TestProjectNeedsAName(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	if code, _ := f.do(t, "POST", "/api/projects", `{"name":"   "}`); code == http.StatusOK {
		t.Error("a blank project name was accepted")
	}
}

func TestAssignAndUnassignASession(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedSession(t, f, "s1")
	id := mkProject(t, f, "Infra")

	if code, body := f.do(t, "POST", "/api/sessions/s1/project", `{"project":"`+id+`"}`); code != http.StatusOK {
		t.Fatalf("assign = %d %v", code, body)
	}
	_, body := f.do(t, "GET", "/api/sessions/s1", "")
	sess, _ := body["session"].(map[string]any)
	if sess["project"] != id {
		t.Fatalf("session not in the project: %v", sess)
	}
	// "" takes it back out.
	if code, _ := f.do(t, "POST", "/api/sessions/s1/project", `{"project":""}`); code != http.StatusOK {
		t.Fatal("unassign failed")
	}
	_, body = f.do(t, "GET", "/api/sessions/s1", "")
	sess, _ = body["session"].(map[string]any)
	if _, ok := sess["project"]; ok {
		t.Errorf("still grouped after unassign: %v", sess)
	}
}

func TestAssignToAnUnknownProjectIsRefused(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedSession(t, f, "s1")
	if code, _ := f.do(t, "POST", "/api/sessions/s1/project", `{"project":"nope"}`); code == http.StatusOK {
		t.Error("assigned a session to a project that does not exist")
	}
}

// Deleting a grouping must not take the conversations with it — the
// label is disposable, the work is not.
func TestDeletingAProjectKeepsItsSessions(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedSession(t, f, "s1")
	id := mkProject(t, f, "Temp")
	f.do(t, "POST", "/api/sessions/s1/project", `{"project":"`+id+`"}`)

	if code, _ := f.do(t, "DELETE", "/api/projects/"+id, ""); code != http.StatusOK {
		t.Fatal("delete failed")
	}
	code, body := f.do(t, "GET", "/api/sessions/s1", "")
	if code != http.StatusOK {
		t.Fatalf("the session died with its project: %d", code)
	}
	sess, _ := body["session"].(map[string]any)
	if _, ok := sess["project"]; ok {
		t.Errorf("session still points at a deleted project: %v", sess)
	}
	if code, _ := f.do(t, "DELETE", "/api/projects/"+id, ""); code == http.StatusOK {
		t.Error("deleting a gone project reported success")
	}
}
