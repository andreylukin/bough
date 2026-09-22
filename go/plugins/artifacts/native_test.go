package artifacts

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
)

// The native artifact tools are the Store's own methods: publish, patch
// statement by statement, read answers, read the guide, and a refusal
// reaches the model as the call's error.
func TestNativeArtifactTools(t *testing.T) {
	t.Parallel()
	s := newStore(t)
	s.web.Handle("/artifacts/", s)
	defer s.web.Unhandle("/artifacts/")
	reg := agenttools.NewRegistry()
	if _, err := agenttools.RegisterAll(reg, s.nativeTools()...); err != nil {
		t.Fatal(err)
	}
	call := func(name, args string) agenttools.Result {
		t.Helper()
		tl, ok := reg.Lookup(name)
		if !ok {
			t.Fatalf("no native %s", name)
		}
		r, err := tl.Call(context.Background(), agenttools.Call{Args: json.RawMessage(args)})
		if err != nil {
			t.Fatal(err)
		}
		return r
	}
	code, _ := json.Marshal(sample)
	url := s.web.URL() + "/artifacts/s1/db"
	if r := call("artifact", `{"name":"db","code":`+string(code)+`}`); r.Error != "" || r.Text != url {
		t.Fatalf("artifact = %+v", r)
	}
	if r := call("artifact_patch", `{"name":"db","statements":"head = CardHeader(\"DB\", \"two\")"}`); r.Error != "" || r.Text != url {
		t.Fatalf("artifact_patch = %+v", r)
	}
	if r := call("artifact_answers", `{"name":"db"}`); r.Text != "no answers yet on db" {
		t.Fatalf("artifact_answers = %+v", r)
	}
	if r := call("artifact_guide", `{}`); r.Error != "" || len(r.Text) < 100 {
		t.Fatalf("artifact_guide = %d bytes, %q", len(r.Text), r.Error)
	}
	if r := call("artifact_patch", `{"name":"nope","statements":"a = 1"}`); !strings.Contains(r.Error, "not published") {
		t.Fatalf("patch of an unknown page = %+v", r)
	}
	tl, _ := reg.Lookup("artifact")
	if d := tl.Detail(json.RawMessage(`{"name":"db","code":"x"}`)); d != "db" {
		t.Fatalf("detail = %q", d)
	}
}
