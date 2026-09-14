package lsp

import (
	"bufio"
	"encoding/json/v2"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/kernel"
	_ "github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/tools"
)

// The test binary doubles as a fake language server for ".fake" files:
// "def name" lines define symbols, a line containing BROKEN is an error.
func TestMain(m *testing.M) {
	if os.Getenv("BOUGH_LSP_FAKE") == "1" {
		fakeServer()
		return
	}
	os.Exit(m.Run())
}

func fakeServer() {
	r := bufio.NewReader(os.Stdin)
	docs := map[string]string{}
	send := func(v any) {
		body, _ := json.Marshal(v)
		fmt.Fprintf(os.Stdout, "Content-Length: %d\r\n\r\n%s", len(body), body)
	}
	wordAt := func(text string, p position) string {
		lines := strings.Split(text, "\n")
		if p.Line >= len(lines) {
			return ""
		}
		return regexp.MustCompile(`^\w+`).FindString(lines[p.Line][p.Character:])
	}
	loc := func(uri string, line, col, n int) map[string]any {
		return map[string]any{"uri": uri, "range": lspRange{position{line, col}, position{line, col + n}}}
	}
	for {
		body, err := readFrame(r)
		if err != nil {
			return
		}
		var m message
		_ = json.Unmarshal(body, &m)
		var p struct {
			TextDocument struct {
				URI  string `json:"uri"`
				Text string `json:"text"`
			} `json:"textDocument"`
			ContentChanges []struct {
				Text string `json:"text"`
			} `json:"contentChanges"`
			Position position `json:"position"`
			Query    string   `json:"query"`
		}
		_ = json.Unmarshal(m.Params, &p)
		reply := func(res any) { send(map[string]any{"jsonrpc": "2.0", "id": m.ID, "result": res}) }
		uri := p.TextDocument.URI
		switch m.Method {
		case "initialize":
			reply(map[string]any{"capabilities": map[string]any{}})
		case "shutdown":
			reply(nil)
		case "exit":
			return
		case "textDocument/didOpen", "textDocument/didChange":
			if m.Method == "textDocument/didOpen" {
				docs[uri] = p.TextDocument.Text
			} else {
				docs[uri] = p.ContentChanges[0].Text
			}
			diags := []diagnostic{}
			for i, l := range strings.Split(docs[uri], "\n") {
				if strings.Contains(l, "BROKEN") {
					diags = append(diags, diagnostic{Range: lspRange{Start: position{i, 0}}, Severity: 1, Message: "broken here", Source: "fake"})
				}
			}
			send(map[string]any{"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
				"params": map[string]any{"uri": uri, "diagnostics": diags}})
		case "textDocument/definition", "textDocument/references", "textDocument/hover":
			w := wordAt(docs[uri], p.Position)
			var out []map[string]any
			for u, text := range docs {
				for i, l := range strings.Split(text, "\n") {
					if m.Method == "textDocument/definition" && strings.HasPrefix(l, "def "+w) {
						out = append(out, loc(u, i, 4, len(w)))
					}
					if j := strings.Index(l, w); m.Method == "textDocument/references" && w != "" && j >= 0 {
						out = append(out, loc(u, i, j, len(w)))
					}
				}
			}
			if m.Method == "textDocument/hover" {
				reply(map[string]any{"contents": map[string]string{"kind": "plaintext", "value": "hover " + w}})
			} else {
				reply(out)
			}
		case "textDocument/documentSymbol", "workspace/symbol":
			var out []map[string]any
			for u, text := range docs {
				if m.Method == "textDocument/documentSymbol" && u != uri {
					continue
				}
				for i, l := range strings.Split(text, "\n") {
					name, ok := strings.CutPrefix(l, "def ")
					if !ok || !strings.Contains(name, p.Query) {
						continue
					}
					if m.Method == "workspace/symbol" {
						out = append(out, map[string]any{"name": name, "kind": 12, "location": loc(u, i, 4, len(name))})
					} else {
						out = append(out, map[string]any{"name": name, "kind": 12, "range": lspRange{position{i, 0}, position{i + 1, 0}}})
					}
				}
			}
			reply(out)
		default:
			if len(m.ID) > 0 {
				reply(nil)
			}
		}
	}
}

type codeRunner interface {
	Run(code string) (string, error)
}

// mountFake mounts codemode + tools + lsp over a temp project holding
// files, with the fake server as the only language, and chdirs into it.
func mountFake(t *testing.T, files map[string]string) (codeRunner, string) {
	t.Helper()
	dir := t.TempDir()
	dir, _ = filepath.EvalSymlinks(dir)
	for name, text := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(text), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	t.Chdir(dir)
	t.Setenv("BOUGH_LSP_FAKE", "1")
	t.Setenv(iorb.WriteRootsEnv, dir)
	saved := languages
	languages = []*language{{name: "fake", exts: map[string]string{".fake": "fake"}, markers: []string{"fake.toml"}, argv: [][]string{{os.Args[0]}}}}
	t.Cleanup(func() { languages = saved })

	ctx := kernel.NewContext()
	if err := ctx.Mount([]kernel.Row{
		{ID: "codemode", Plugin: "codemode"},
		{ID: "tools", Plugin: "tools-basic"},
		{ID: "lsp", Plugin: "lsp"},
	}); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	cm, err := kernel.Get[codeRunner](ctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}
	return cm, dir
}

func run(t *testing.T, cm codeRunner, code string) string {
	t.Helper()
	out, err := cm.Run(code)
	if err != nil {
		t.Fatalf("%s: %v", code, err)
	}
	return out
}

const mainFake = "def helper_fn\n\ncall helper_fn here\nand helper_fn again\n"

func TestNavigation(t *testing.T) {
	cm, _ := mountFake(t, map[string]string{"fake.toml": "", "main.fake": mainFake})

	if got := run(t, cm, `tools.lsp.def("main.fake", "helper_fn", 3)`); got != "main.fake:1│def helper_fn" {
		t.Errorf("def = %q", got)
	}
	if got := run(t, cm, `tools.lsp.refs("main.fake", "helper_fn")`); !strings.HasPrefix(got, "3 references\n") || !strings.Contains(got, "main.fake:4│and helper_fn again") {
		t.Errorf("refs = %q", got)
	}
	if got := run(t, cm, `tools.lsp.hover("main.fake", "helper_fn", 4)`); got != "hover helper_fn" {
		t.Errorf("hover = %q", got)
	}
	if got := run(t, cm, `tools.lsp.outline("main.fake")`); !strings.Contains(got, "1-2 function helper_fn") {
		t.Errorf("outline = %q", got)
	}
	if got := run(t, cm, `tools.lsp.symbols("helper")`); got != "function helper_fn main.fake:1" {
		t.Errorf("symbols = %q", got)
	}
	if got := run(t, cm, `tools.lsp.diagnostics("main.fake")`); got != "main.fake: no errors or warnings" {
		t.Errorf("diagnostics = %q", got)
	}
	// A symbol that is not on the file is the model's error, said plainly.
	if _, err := cm.Run(`tools.lsp.def("main.fake", "nope_fn")`); err == nil || !strings.Contains(err.Error(), `"nope_fn" does not occur`) {
		t.Errorf("missing symbol: %v", err)
	}
}

func TestEditsReportErrors(t *testing.T) {
	cm, _ := mountFake(t, map[string]string{"fake.toml": "", "main.fake": mainFake})

	got := run(t, cm, `tools.patch("main.fake", "call helper_fn here", "call BROKEN here")`)
	if !strings.Contains(got, "[lsp] 1 errors in main.fake") || !strings.Contains(got, "main.fake:3:1 error: broken here (fake)") {
		t.Fatalf("patch = %q", got)
	}
	// An edit made outside the tools (bash) is picked up on the next sync.
	if err := os.WriteFile("main.fake", []byte(mainFake), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := run(t, cm, `tools.lsp.diagnostics("main.fake")`); got != "main.fake: no errors or warnings" {
		t.Errorf("after bash edit: %q", got)
	}
	if got := run(t, cm, `tools.write("new.fake", "BROKEN\nBROKEN\n")`); !strings.Contains(got, "[lsp] 2 errors in new.fake") {
		t.Errorf("write = %q", got)
	}
	// No server for the file: the edit result carries nothing extra.
	if got := run(t, cm, `tools.write("notes.txt", "hi\n")`); strings.Contains(got, "[lsp]") {
		t.Errorf("txt write = %q", got)
	}
}

func TestBashGrepNote(t *testing.T) {
	cm, _ := mountFake(t, map[string]string{"fake.toml": "", "main.fake": mainFake})

	got := run(t, cm, `tools.bash("grep -n 'helper_fn' main.fake")`)
	if !strings.Contains(got, `[lsp] helper_fn looks like a symbol`) {
		t.Fatalf("first grep = %q", got)
	}
	if got := run(t, cm, `tools.bash("grep -n helper_fn main.fake")`); strings.Contains(got, "[lsp]") {
		t.Errorf("second grep nudged again: %q", got)
	}
	for _, cmd := range []string{"grep -rn 'connection refused' . || true", "echo helper_fn", "grep -n def main.fake"} {
		if got := run(t, cm, fmt.Sprintf("tools.bash(%q)", cmd)); strings.Contains(got, "[lsp]") {
			t.Errorf("%s nudged: %q", cmd, got)
		}
	}
}

func TestNoServerInstalled(t *testing.T) {
	cm, _ := mountFake(t, map[string]string{"fake.toml": "", "main.fake": mainFake})
	languages[0].argv = [][]string{{"bough-no-such-langserver"}}

	_, err := cm.Run(`tools.lsp.def("main.fake", "helper_fn")`)
	if err == nil || !strings.Contains(err.Error(), "no fake language server installed (tried bough-no-such-langserver); use grep") {
		t.Fatalf("err = %v", err)
	}
	if _, err := cm.Run(`tools.lsp.outline("README")`); err == nil || !strings.Contains(err.Error(), "no such file") {
		t.Errorf("missing file: %v", err)
	}
	if got := run(t, cm, `tools.bash("grep -n helper_fn main.fake")`); strings.Contains(got, "[lsp]") {
		t.Errorf("nudged with no server: %q", got)
	}
}

func TestLocate(t *testing.T) {
	text := "héllo wide_fn\nwide_fnx wide_fn\n"
	for _, c := range []struct {
		line int
		want position
	}{
		{0, position{0, 6}}, // UTF-16 columns: é is one unit
		{2, position{1, 9}}, // whole word only: wide_fnx is skipped
		{9, position{1, 9}}, // past the end: nearest line that has it
	} {
		got, err := locate(text, "wide_fn", c.line)
		if err != nil || got != c.want {
			t.Errorf("line %d: %v %v, want %v", c.line, got, err, c.want)
		}
	}
}

func TestRootFor(t *testing.T) {
	dir := t.TempDir()
	for _, d := range []string{".git", "crates/a/src"} {
		os.MkdirAll(filepath.Join(dir, d), 0o755)
	}
	os.WriteFile(filepath.Join(dir, "Cargo.toml"), nil, 0o644)
	os.WriteFile(filepath.Join(dir, "crates/a/Cargo.toml"), nil, 0o644)
	rust := &language{markers: []string{"Cargo.toml"}, outermost: true}
	py := &language{markers: []string{"Cargo.toml"}}
	if got := rootFor(rust, filepath.Join(dir, "crates/a/src")); got != dir {
		t.Errorf("outermost = %s", got)
	}
	if got := rootFor(py, filepath.Join(dir, "crates/a/src")); got != filepath.Join(dir, "crates/a") {
		t.Errorf("nearest = %s", got)
	}
}

// TestPyrightLive drives the real server when one is installed.
func TestPyrightLive(t *testing.T) {
	argv := languages[0].command()
	if argv == nil || testing.Short() {
		t.Skip("no pyright language server installed")
	}
	cm, _ := func() (codeRunner, string) {
		saved := languages
		cm, dir := mountFake(t, map[string]string{
			"pyproject.toml": "[project]\nname = \"x\"\n",
			"a.py":           "def helper_fn(x: int) -> int:\n    return x\n",
			"b.py":           "from a import helper_fn\n\nhelper_fn(1)\n",
		})
		languages = saved // the real table, not the fake
		return cm, dir
	}()
	diagWait = 20 * time.Second

	if got := run(t, cm, `tools.lsp.def("b.py", "helper_fn", 3)`); got != "a.py:1│def helper_fn(x: int) -> int:" {
		t.Errorf("def = %q", got)
	}
	if got := run(t, cm, `tools.lsp.hover("b.py", "helper_fn", 3)`); !strings.Contains(got, "(x: int) -> int") {
		t.Errorf("hover = %q", got)
	}
	got := run(t, cm, `tools.patch("b.py", "helper_fn(1)", "helper_fn(1)\nmissing_name")`)
	if !strings.Contains(got, "errors in b.py") || !strings.Contains(got, "missing_name") {
		t.Errorf("patch = %q", got)
	}
}
