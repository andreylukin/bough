package lsp

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/agenttools"
)

// The native lsp is tools.lsp by op, and a native patch still ends with
// the file's errors: the after-edit hook sits under both paths.
// Not parallel: mountFakeRows chdirs and sets env.
func TestNativeLSP(t *testing.T) {
	ctx, _ := mountFakeRows(t, map[string]string{"fake.toml": "", "main.fake": mainFake},
		kernel.Row{ID: "agent-tools", Plugin: "agent-tools"})
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
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
	if r := call("lsp", `{"op":"def","path":"main.fake","symbol":"helper_fn","line":3}`); r.Text != "main.fake:1│def helper_fn" {
		t.Fatalf("def = %+v", r)
	}
	if r := call("lsp", `{"op":"symbols","query":"helper"}`); r.Text != "function helper_fn main.fake:1" {
		t.Fatalf("symbols = %+v", r)
	}
	if r := call("lsp", `{"op":"def","path":"main.fake","symbol":"nope_fn"}`); !strings.Contains(r.Error, `"nope_fn" does not occur`) {
		t.Fatalf("missing symbol = %+v", r)
	}
	if r := call("lsp", `{"op":"rename"}`); !strings.Contains(r.Error, "op must be") {
		t.Fatalf("bad op = %+v", r)
	}
	r := call("patch", `{"path":"main.fake","old":"call helper_fn here","new":"call BROKEN here"}`)
	if r.Error != "" || !strings.Contains(r.Text, "[lsp] 1 errors in main.fake") {
		t.Fatalf("native patch = %+v", r)
	}
	tl, _ := reg.Lookup("lsp")
	if d := tl.Detail(json.RawMessage(`{"op":"refs","path":"a.go","symbol":"Run"}`)); d != "refs a.go Run" {
		t.Fatalf("detail = %q", d)
	}
}
