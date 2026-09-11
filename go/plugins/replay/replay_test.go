package replay

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/llm"
)

const tape = `{"seq":1,"kind":"meta","data":{"cwd":"/x"}}
{"seq":2,"kind":"input","data":{"text":"list files"}}
{"seq":3,"kind":"assistant","data":{"text":"` + "```js\\nconsole.log(tools.bash('ls'))\\n```" + `"}}
{"seq":4,"kind":"code","data":{"text":"console.log(tools.bash('ls'))\n"}}
{"seq":5,"kind":"result","data":{"code":"console.log(tools.bash('ls'))\n","text":"a.go\nb.go\n"}}
{"seq":6,"kind":"sub:assistant","data":{"text":"never replayed"}}
{"seq":7,"kind":"assistant","data":{"text":"` + "```js\\ntools.bash('false')\\n```" + `"}}
{"seq":8,"kind":"result","data":{"code":"tools.bash('false')\n","text":"error: exit status 1"}}
{"seq":9,"kind":"assistant","data":{"text":"` + "```stop\\ntwo files\\n```" + `"}}
{"seq":10,"kind":"done","data":{"text":""}}
`

func write(t *testing.T) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "s.jsonl")
	if err := os.WriteFile(p, []byte(tape), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestTapeReplaysInOrder(t *testing.T) {
	tp, err := Load(write(t))
	if err != nil {
		t.Fatal(err)
	}
	if tp.Turns() != 1 || len(tp.Replies) != 3 || len(tp.Results) != 2 {
		t.Fatalf("tape: %d turns, %d replies, %d results", tp.Turns(), len(tp.Replies), len(tp.Results))
	}
	m := &Model{tape: tp}
	rt := &Runtime{CodeMode: codemode.New(time.Second), tape: tp}
	r1, _ := m.Complete(t.Context(), "", nil)
	if !strings.Contains(r1, "tools.bash('ls')") {
		t.Fatalf("first reply: %q", r1)
	}
	out, err := rt.Run("console.log(tools.bash('ls'))\n")
	if err != nil || out != "a.go\nb.go\n" {
		t.Fatalf("first result: %q %v", out, err)
	}
	var got []string
	if _, err := m.Stream(t.Context(), "", nil, func(d string) { got = append(got, d) }); err != nil {
		t.Fatal(err)
	}
	if strings.Join(got, "") != "```js\ntools.bash('false')\n```" || len(got) < 3 {
		t.Fatalf("stream deltas: %q", got)
	}
	if _, err := rt.Run("tools.bash('false')\n"); err == nil || err.Error() != "exit status 1" {
		t.Fatalf("recorded error not surfaced: %v", err)
	}
	r3, _ := m.Complete(t.Context(), "", nil)
	if !strings.Contains(r3, "two files") {
		t.Fatalf("third reply: %q", r3)
	}
	r4, _ := m.Complete(t.Context(), "", nil)
	if !strings.Contains(r4, "end of tape") {
		t.Fatalf("past the end: %q", r4)
	}
}

func TestPluginProvidesBothSides(t *testing.T) {
	p := write(t)
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, map[string]any{"file": p}); err != nil {
		t.Fatal(err)
	}
	if err := (plugin{}).Apply(ctx, map[string]any{"file": p, "provide": "codemode"}); err != nil {
		t.Fatal(err)
	}
	if _, err := kernel.Get[llm.LLM](ctx, "llm"); err != nil {
		t.Fatal(err)
	}
	if _, err := kernel.Get[*Runtime](ctx, "codemode"); err != nil {
		t.Fatal(err)
	}
	if err := (plugin{}).Apply(kernel.NewContext(), nil); err == nil {
		t.Fatal("no file: want an error")
	}
}
