// Package lsp is the "lsp" plugin: language servers on the host
// (basedpyright/pyright, rust-analyzer, typescript-language-server,
// gopls — whichever are installed), started lazily per language and
// project root and kept for the session. It registers tools.lsp (def,
// refs, hover, outline, symbols, diagnostics), a prompt section that
// steers the agent to them before grep, the tools row's after-edit hook
// (a file's errors follow every write and patch) and a bash note when a
// grep looks for a symbol.
//
// Servers always run on the host. A project session's worktrees are
// bind-mounted at their host paths, so the same paths work; a Python
// venv built inside the orb is never executed, only its site-packages
// are handed to pyright as extra search paths.
package lsp

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"maps"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"sync"
	"time"
	"unicode/utf16"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

// language is one kind of server: which files it takes, what marks its
// project root, and the commands that start it (first installed wins).
type language struct {
	name    string
	exts    map[string]string // extension -> LSP languageId
	markers []string
	argv    [][]string
	// outermost: the root is the highest marker below the repository
	// (a Cargo workspace), not the nearest.
	outermost bool
	// python: hand pyright the session's venv site-packages.
	python bool
}

// languages is a var so tests can swap in a fake server.
var languages = []*language{
	{name: "python", exts: map[string]string{".py": "python"},
		markers: []string{"pyrightconfig.json", "pyproject.toml", "setup.py", "setup.cfg", "requirements.txt"},
		argv:    [][]string{{"basedpyright-langserver", "--stdio"}, {"pyright-langserver", "--stdio"}},
		python:  true},
	{name: "rust", exts: map[string]string{".rs": "rust"},
		markers: []string{"Cargo.toml"}, argv: [][]string{{"rust-analyzer"}}, outermost: true},
	{name: "typescript", exts: map[string]string{".ts": "typescript", ".tsx": "typescriptreact", ".js": "javascript", ".jsx": "javascriptreact", ".mjs": "javascript", ".cjs": "javascript"},
		markers: []string{"tsconfig.json", "jsconfig.json", "package.json"},
		argv:    [][]string{{"typescript-language-server", "--stdio"}}},
	{name: "go", exts: map[string]string{".go": "go"},
		markers: []string{"go.work", "go.mod"}, argv: [][]string{{"gopls"}}},
}

// Timeouts; vars so tests can shorten them.
var (
	startTimeout   = 30 * time.Second
	requestTimeout = 20 * time.Second
	diagWait       = 6 * time.Second
)

const maxItems = 50

func languageFor(path string) *language {
	ext := strings.ToLower(filepath.Ext(path))
	for _, l := range languages {
		if _, ok := l.exts[ext]; ok {
			return l
		}
	}
	return nil
}

// command is the first of l's commands on PATH, or nil.
func (l *language) command() []string {
	for _, a := range l.argv {
		if p, err := exec.LookPath(a[0]); err == nil {
			return append([]string{p}, a[1:]...)
		}
	}
	return nil
}

// rootFor walks up from dir to the repository top (a .git entry) or the
// filesystem root, and picks the nearest marker directory (the highest,
// for an outermost language). No marker: the repository top, else dir.
func rootFor(l *language, dir string) string {
	found, top := "", ""
	for d := dir; ; d = filepath.Dir(d) {
		for _, m := range l.markers {
			if _, err := os.Stat(filepath.Join(d, m)); err == nil {
				if found == "" || l.outermost {
					found = d
				}
				break
			}
		}
		if _, err := os.Stat(filepath.Join(d, ".git")); err == nil {
			top = d
			break
		}
		if filepath.Dir(d) == d {
			break
		}
	}
	return cmp.Or(found, top, dir)
}

type position struct {
	Line      int `json:"line"`
	Character int `json:"character"`
}

type lspRange struct {
	Start position `json:"start"`
	End   position `json:"end"`
}

type location struct {
	URI   string   `json:"uri"`
	Range lspRange `json:"range"`
	// LocationLink's fields, for servers that answer definition with links.
	TargetURI            string   `json:"targetUri"`
	TargetSelectionRange lspRange `json:"targetSelectionRange"`
}

type diagnostic struct {
	Range    lspRange `json:"range"`
	Severity int      `json:"severity"`
	Message  string   `json:"message"`
	Source   string   `json:"source"`
}

type doc struct {
	version int
	text    string
}

// server is one running language server for one root.
type server struct {
	lang *language
	root string
	conn *conn

	mu       sync.Mutex
	cond     *sync.Cond
	docs     map[string]*doc // by uri
	diags    map[string][]diagnostic
	diagSeq  map[string]int // publishes per uri, to wait for a fresh one
	progress map[string]bool
	ready    chan struct{} // closed once initialize answered
	initErr  error
}

func fileURI(path string) string {
	return (&url.URL{Scheme: "file", Path: path}).String()
}

func uriPath(uri string) string {
	if u, err := url.Parse(uri); err == nil && u.Scheme == "file" {
		return u.Path
	}
	return uri
}

func (s *server) start(argv []string) error {
	s.cond = sync.NewCond(&s.mu)
	s.docs, s.diags, s.diagSeq, s.progress = map[string]*doc{}, map[string][]diagnostic{}, map[string]int{}, map[string]bool{}
	c, err := dial(s.root, argv, s.onNotify, s.onRequest)
	if err != nil {
		return err
	}
	s.conn = c
	folder := []map[string]string{{"uri": fileURI(s.root), "name": filepath.Base(s.root)}}
	params := map[string]any{
		"processId":        os.Getpid(),
		"rootUri":          fileURI(s.root),
		"rootPath":         s.root,
		"workspaceFolders": folder,
		"capabilities": map[string]any{
			"textDocument": map[string]any{
				"synchronization":    map[string]any{"didSave": true},
				"publishDiagnostics": map[string]any{"versionSupport": true},
				"definition":         map[string]any{"linkSupport": true},
				"hover":              map[string]any{"contentFormat": []string{"plaintext", "markdown"}},
				"documentSymbol":     map[string]any{"hierarchicalDocumentSymbolSupport": true},
			},
			"workspace": map[string]any{"configuration": true, "workspaceFolders": true, "symbol": map[string]any{}},
			"window":    map[string]any{"workDoneProgress": true},
		},
	}
	ctx, cancel := context.WithTimeout(context.Background(), startTimeout)
	defer cancel()
	if err := c.call(ctx, "initialize", params, nil); err != nil {
		c.close()
		return fmt.Errorf("%s: initialize: %w", filepath.Base(argv[0]), err)
	}
	return c.notify("initialized", map[string]any{})
}

func (s *server) onNotify(method string, params jsontext.Value) {
	switch method {
	case "textDocument/publishDiagnostics":
		var p struct {
			URI         string       `json:"uri"`
			Diagnostics []diagnostic `json:"diagnostics"`
		}
		if json.Unmarshal(params, &p) != nil {
			return
		}
		s.mu.Lock()
		s.diags[p.URI] = p.Diagnostics
		s.diagSeq[p.URI]++
		s.cond.Broadcast()
		s.mu.Unlock()
	case "$/progress":
		var p struct {
			Token jsontext.Value `json:"token"`
			Value struct {
				Kind string `json:"kind"`
			} `json:"value"`
		}
		if json.Unmarshal(params, &p) != nil {
			return
		}
		s.mu.Lock()
		switch p.Value.Kind {
		case "begin":
			s.progress[string(p.Token)] = true
		case "end":
			delete(s.progress, string(p.Token))
		}
		s.mu.Unlock()
	}
}

func (s *server) onRequest(method string, params jsontext.Value) any {
	switch method {
	case "workspace/configuration":
		var p struct {
			Items []struct {
				Section string `json:"section"`
			} `json:"items"`
		}
		_ = json.Unmarshal(params, &p)
		out := make([]any, len(p.Items))
		for i, it := range p.Items {
			out[i] = s.settings(it.Section)
		}
		return out
	case "workspace/workspaceFolders":
		return []map[string]string{{"uri": fileURI(s.root), "name": filepath.Base(s.root)}}
	}
	return nil
}

// settings answers workspace/configuration. Only Python has any: the
// venv's site-packages as extra search paths, and diagnostics for open
// files only (a whole-workspace pass on a big repo is minutes of CPU).
func (s *server) settings(section string) any {
	if !s.lang.python {
		return nil
	}
	analysis := map[string]any{"diagnosticMode": "openFilesOnly", "extraPaths": sitePackages(s.root)}
	switch section {
	case "python.analysis", "basedpyright.analysis":
		return analysis
	case "python", "basedpyright":
		return map[string]any{"analysis": analysis}
	}
	return nil
}

// sitePackages finds venvs pyright would not: the root's own, and the
// scratchpad's (where a project session's setup builds one).
func sitePackages(root string) []string {
	var out []string
	for _, base := range []string{root, os.Getenv("BOUGH_SCRATCH"), os.Getenv("VIRTUAL_ENV")} {
		if base == "" {
			continue
		}
		for _, pat := range []string{"lib/python3*/site-packages", "*/lib/python3*/site-packages"} {
			m, _ := filepath.Glob(filepath.Join(base, pat))
			out = append(out, m...)
		}
	}
	slices.Sort(out)
	return slices.Compact(out)
}

func (s *server) indexing() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.progress) > 0
}

// sync opens path, or sends its new text when it changed on disk since
// (an edit by any tool, bash included). It returns the uri, the text,
// and the uri's publish count before anything was sent.
func (s *server) sync(path string) (string, string, int, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", "", 0, err
	}
	text, uri := string(data), fileURI(path)
	s.mu.Lock()
	d, seq := s.docs[uri], s.diagSeq[uri]
	switch {
	case d == nil:
		s.docs[uri] = &doc{version: 1, text: text}
		s.mu.Unlock()
		id := s.lang.exts[strings.ToLower(filepath.Ext(path))]
		err = s.conn.notify("textDocument/didOpen", map[string]any{"textDocument": map[string]any{
			"uri": uri, "languageId": id, "version": 1, "text": text}})
	case d.text != text:
		d.version++
		d.text = text
		v := d.version
		s.mu.Unlock()
		err = s.conn.notify("textDocument/didChange", map[string]any{
			"textDocument":   map[string]any{"uri": uri, "version": v},
			"contentChanges": []map[string]string{{"text": text}}})
		if err == nil {
			err = s.conn.notify("textDocument/didSave", map[string]any{"textDocument": map[string]any{"uri": uri}})
		}
	default:
		s.mu.Unlock()
		return uri, text, -1, nil // unchanged: current diagnostics stand
	}
	return uri, text, seq, err
}

// diagnostics syncs path and returns its diagnostics, waiting up to wait
// for a publish newer than the sync. fresh is false when none came.
func (s *server) diagnostics(path string, wait time.Duration) ([]diagnostic, bool, error) {
	uri, _, seq, err := s.sync(path)
	if err != nil {
		return nil, false, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if seq < 0 {
		_, seen := s.diagSeq[uri]
		if seen {
			return s.diags[uri], true, nil
		}
		seq = 0
	}
	deadline := time.Now().Add(wait)
	timer := time.AfterFunc(wait, func() {
		s.mu.Lock()
		s.cond.Broadcast()
		s.mu.Unlock()
	})
	defer timer.Stop()
	for s.diagSeq[uri] <= seq && time.Now().Before(deadline) && s.conn.alive() {
		s.cond.Wait()
	}
	return s.diags[uri], s.diagSeq[uri] > seq, nil
}

// Manager owns the session's servers.
type Manager struct {
	mu      sync.Mutex
	servers map[string]*server // lang name + root
	missing map[string]bool    // languages with no server installed
	nudged  map[string]bool    // symbols the bash note already named
}

func newManager() *Manager {
	return &Manager{servers: map[string]*server{}, missing: map[string]bool{}, nudged: map[string]bool{}}
}

// serverFor returns the running server for path's language and root,
// starting it on first use. (nil, nil) means no language or no server
// installed for it.
func (m *Manager) serverFor(path string) (*server, error) {
	l := languageFor(path)
	if l == nil {
		return nil, nil
	}
	root := rootFor(l, filepath.Dir(path))
	key := l.name + "\x00" + root
	m.mu.Lock()
	s := m.servers[key]
	if s == nil || (s.conn != nil && !s.conn.alive()) {
		if m.missing[l.name] {
			m.mu.Unlock()
			return nil, nil
		}
		argv := l.command()
		if argv == nil {
			m.missing[l.name] = true
			m.mu.Unlock()
			return nil, nil
		}
		s = &server{lang: l, root: root, ready: make(chan struct{})}
		m.servers[key] = s
		m.mu.Unlock()
		s.initErr = s.start(argv)
		close(s.ready)
		if s.initErr != nil {
			m.mu.Lock()
			delete(m.servers, key)
			m.mu.Unlock()
		}
		return s, s.initErr
	}
	m.mu.Unlock()
	<-s.ready
	return s, s.initErr
}

func (m *Manager) close() {
	m.mu.Lock()
	all := slices.Collect(maps.Values(m.servers))
	clear(m.servers)
	m.mu.Unlock()
	var wg sync.WaitGroup
	for _, s := range all {
		wg.Go(func() {
			<-s.ready
			if s.initErr == nil {
				s.conn.close()
			}
		})
	}
	wg.Wait()
}

func absPath(path string) (string, error) {
	if path == "" {
		return "", fmt.Errorf("lsp: no path")
	}
	return filepath.Abs(path)
}

// need is serverFor for a tool call: no server is an error that says why.
func (m *Manager) need(path string) (*server, string, error) {
	abs, err := absPath(path)
	if err != nil {
		return nil, "", err
	}
	if _, err := os.Stat(abs); err != nil {
		return nil, "", fmt.Errorf("lsp: %w", err)
	}
	s, err := m.serverFor(abs)
	if err != nil {
		return nil, "", fmt.Errorf("lsp: %w", err)
	}
	if s == nil {
		if l := languageFor(abs); l != nil {
			return nil, "", fmt.Errorf("lsp: no %s language server installed (tried %s); use grep", l.name, argvNames(l))
		}
		return nil, "", fmt.Errorf("lsp: no language server for %s files; use grep", cmp.Or(filepath.Ext(abs), filepath.Base(abs)))
	}
	return s, abs, nil
}

func argvNames(l *language) string {
	var names []string
	for _, a := range l.argv {
		names = append(names, a[0])
	}
	return strings.Join(names, ", ")
}

var wordChar = regexp.MustCompile(`[\p{L}\p{N}_$]`)

// locate finds symbol as a whole word in text — on line (1-based) or the
// nearest line to it, or its first occurrence when line is 0 — and
// returns the LSP position of its start (UTF-16 columns).
func locate(text, symbol string, line int) (position, error) {
	if symbol == "" {
		return position{}, fmt.Errorf("lsp: no symbol")
	}
	lines := strings.Split(text, "\n")
	col := func(i int) int {
		l := lines[i]
		for from := 0; ; {
			j := strings.Index(l[from:], symbol)
			if j < 0 {
				return -1
			}
			j += from
			end := j + len(symbol)
			before := j > 0 && wordChar.MatchString(l[j-1:j])
			after := end < len(l) && wordChar.MatchString(l[end:end+1])
			if !before && !after {
				return j
			}
			from = j + 1
		}
	}
	at := func(i, j int) position {
		return position{Line: i, Character: len(utf16.Encode([]rune(lines[i][:j])))}
	}
	if line <= 0 {
		for i := range lines {
			if j := col(i); j >= 0 {
				return at(i, j), nil
			}
		}
		return position{}, fmt.Errorf("lsp: %q does not occur in the file", symbol)
	}
	target := min(line-1, len(lines)-1)
	for d := 0; d < len(lines); d++ {
		for _, i := range []int{target - d, target + d} {
			if i >= 0 && i < len(lines) {
				if j := col(i); j >= 0 {
					return at(i, j), nil
				}
			}
		}
		if d > 40 {
			break
		}
	}
	return position{}, fmt.Errorf("lsp: %q is not on or near line %d", symbol, line)
}

// lineText is line (0-based) of path, trimmed, cached per call.
type lineCache map[string][]string

func (c lineCache) text(path string, line int) string {
	ls, ok := c[path]
	if !ok {
		data, _ := os.ReadFile(path)
		ls = strings.Split(string(data), "\n")
		c[path] = ls
	}
	if line < 0 || line >= len(ls) {
		return ""
	}
	return strings.TrimSpace(ls[line])
}

func display(path string) string {
	if wd, err := os.Getwd(); err == nil {
		if rel, err := filepath.Rel(wd, path); err == nil && !strings.HasPrefix(rel, "..") {
			return rel
		}
	}
	return path
}

func formatLocations(locs []location, what string) string {
	if len(locs) == 0 {
		return "(no " + what + " found)"
	}
	cache := lineCache{}
	var b strings.Builder
	for i, l := range locs {
		if i == maxItems {
			fmt.Fprintf(&b, "… %d more\n", len(locs)-maxItems)
			break
		}
		uri, r := l.URI, l.Range
		if l.TargetURI != "" {
			uri, r = l.TargetURI, l.TargetSelectionRange
		}
		p := uriPath(uri)
		fmt.Fprintf(&b, "%s:%d│%s\n", display(p), r.Start.Line+1, cache.text(p, r.Start.Line))
	}
	return strings.TrimRight(b.String(), "\n")
}

// decodeLocations reads Location | Location[] | LocationLink[] | null.
func decodeLocations(raw jsontext.Value) []location {
	raw = jsontext.Value(strings.TrimSpace(string(raw)))
	if len(raw) == 0 || string(raw) == "null" {
		return nil
	}
	if raw[0] == '{' {
		var l location
		if json.Unmarshal(raw, &l) == nil {
			return []location{l}
		}
		return nil
	}
	var ls []location
	_ = json.Unmarshal(raw, &ls)
	return ls
}

// at runs a position request for symbol in path.
func (m *Manager) at(method, path, symbol string, args []any, extra map[string]any) (*server, jsontext.Value, error) {
	s, abs, err := m.need(path)
	if err != nil {
		return nil, nil, err
	}
	_, text, _, err := s.sync(abs)
	if err != nil {
		return nil, nil, err
	}
	line := 0
	if len(args) > 0 {
		line = intOf(args[0])
	}
	pos, err := locate(text, symbol, line)
	if err != nil {
		return nil, nil, err
	}
	params := map[string]any{"textDocument": map[string]any{"uri": fileURI(abs)}, "position": pos}
	for k, v := range extra {
		params[k] = v
	}
	ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
	defer cancel()
	var raw jsontext.Value
	if err := s.conn.call(ctx, method, params, &raw); err != nil {
		return s, nil, fmt.Errorf("lsp: %w%s", err, s.indexingNote())
	}
	return s, raw, nil
}

func (s *server) indexingNote() string {
	if s.indexing() {
		return fmt.Sprintf(" (%s is still indexing %s; try again shortly, or grep meanwhile)", s.lang.name, display(s.root))
	}
	return ""
}

func intOf(v any) int {
	switch n := v.(type) {
	case int:
		return n
	case int64:
		return int(n)
	case float64:
		return int(n)
	}
	return 0
}

func (m *Manager) def(path, symbol string, line ...any) (string, error) {
	s, raw, err := m.at("textDocument/definition", path, symbol, line, nil)
	if err != nil {
		return "", err
	}
	locs := decodeLocations(raw)
	if len(locs) == 0 {
		return "(no definition found)" + s.indexingNote(), nil
	}
	return formatLocations(locs, "definition"), nil
}

func (m *Manager) refs(path, symbol string, line ...any) (string, error) {
	s, raw, err := m.at("textDocument/references", path, symbol, line, map[string]any{"context": map[string]bool{"includeDeclaration": true}})
	if err != nil {
		return "", err
	}
	locs := decodeLocations(raw)
	if len(locs) == 0 {
		return "(no references found)" + s.indexingNote(), nil
	}
	return fmt.Sprintf("%d references\n%s", len(locs), formatLocations(locs, "references")), nil
}

func (m *Manager) hover(path, symbol string, line ...any) (string, error) {
	s, raw, err := m.at("textDocument/hover", path, symbol, line, nil)
	if err != nil {
		return "", err
	}
	var h struct {
		Contents jsontext.Value `json:"contents"`
	}
	if len(raw) == 0 || string(raw) == "null" || json.Unmarshal(raw, &h) != nil {
		return "(no hover information)" + s.indexingNote(), nil
	}
	text := strings.TrimSpace(hoverText(h.Contents))
	if len(text) > 3000 {
		text = text[:3000] + "\n…"
	}
	return cmp.Or(text, "(no hover information)"), nil
}

// hoverText flattens MarkupContent | MarkedString | MarkedString[].
func hoverText(raw jsontext.Value) string {
	var s string
	if json.Unmarshal(raw, &s) == nil {
		return s
	}
	var mc struct {
		Value string `json:"value"`
	}
	if json.Unmarshal(raw, &mc) == nil && mc.Value != "" {
		return mc.Value
	}
	var list []jsontext.Value
	if json.Unmarshal(raw, &list) == nil {
		var parts []string
		for _, p := range list {
			parts = append(parts, hoverText(p))
		}
		return strings.Join(parts, "\n")
	}
	return ""
}

var symbolKinds = []string{"", "file", "module", "namespace", "package", "class", "method", "property", "field", "constructor", "enum", "interface", "function", "variable", "constant", "string", "number", "boolean", "array", "object", "key", "null", "enum member", "struct", "event", "operator", "type parameter"}

func kindName(k int) string {
	if k > 0 && k < len(symbolKinds) {
		return symbolKinds[k]
	}
	return "symbol"
}

type docSymbol struct {
	Name     string      `json:"name"`
	Kind     int         `json:"kind"`
	Range    lspRange    `json:"range"`
	Location *location   `json:"location"` // SymbolInformation form
	Children []docSymbol `json:"children"`
}

func (m *Manager) outline(path string) (string, error) {
	s, abs, err := m.need(path)
	if err != nil {
		return "", err
	}
	if _, _, _, err := s.sync(abs); err != nil {
		return "", err
	}
	ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
	defer cancel()
	var syms []docSymbol
	if err := s.conn.call(ctx, "textDocument/documentSymbol", map[string]any{"textDocument": map[string]any{"uri": fileURI(abs)}}, &syms); err != nil {
		return "", fmt.Errorf("lsp: %w%s", err, s.indexingNote())
	}
	if len(syms) == 0 {
		return "(no symbols)" + s.indexingNote(), nil
	}
	var b strings.Builder
	n := 0
	var walk func([]docSymbol, int)
	walk = func(list []docSymbol, depth int) {
		for _, sym := range list {
			// Locals and fields make an outline a second copy of the file.
			if depth > 1 || sym.Kind == 13 && depth > 0 {
				continue
			}
			r := sym.Range
			if sym.Location != nil {
				r = sym.Location.Range
			}
			if n < 300 {
				fmt.Fprintf(&b, "%s%d-%d %s %s\n", strings.Repeat("  ", depth), r.Start.Line+1, r.End.Line+1, kindName(sym.Kind), sym.Name)
			}
			n++
			walk(sym.Children, depth+1)
		}
	}
	walk(syms, 0)
	if n > 300 {
		fmt.Fprintf(&b, "… %d more\n", n-300)
	}
	return display(abs) + " (lines start-end kind name)\n" + strings.TrimRight(b.String(), "\n"), nil
}

func (m *Manager) symbols(query string, path ...string) (string, error) {
	var servers []*server
	if len(path) > 0 && path[0] != "" {
		s, _, err := m.need(path[0])
		if err != nil {
			return "", err
		}
		servers = []*server{s}
	} else {
		var err error
		if servers, err = m.projectServers(); err != nil {
			return "", err
		}
	}
	if len(servers) == 0 {
		return "", fmt.Errorf("lsp: no language server for this directory; pass a file: tools.lsp.symbols(query, path)")
	}
	var all []docSymbol
	var notes []string
	for _, s := range servers {
		ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
		var syms []docSymbol
		err := s.conn.call(ctx, "workspace/symbol", map[string]string{"query": query}, &syms)
		cancel()
		if err != nil {
			notes = append(notes, fmt.Sprintf("%s: %v", s.lang.name, err))
			continue
		}
		all = append(all, syms...)
		if n := s.indexingNote(); n != "" {
			notes = append(notes, strings.TrimSpace(n))
		}
	}
	var b strings.Builder
	if len(all) == 0 {
		b.WriteString("(no symbols match " + fmt.Sprintf("%q", query) + ")")
	}
	for i, sym := range all {
		if i == maxItems {
			fmt.Fprintf(&b, "… %d more; narrow the query\n", len(all)-maxItems)
			break
		}
		if sym.Location == nil {
			continue
		}
		fmt.Fprintf(&b, "%s %s %s:%d\n", kindName(sym.Kind), sym.Name, display(uriPath(sym.Location.URI)), sym.Location.Range.Start.Line+1)
	}
	for _, n := range notes {
		b.WriteString("\n" + n)
	}
	return strings.TrimRight(b.String(), "\n"), nil
}

// projectServers are the servers for the working directory: the ones
// already running, else one per language whose markers are there.
func (m *Manager) projectServers() ([]*server, error) {
	wd, err := os.Getwd()
	if err != nil {
		return nil, err
	}
	m.mu.Lock()
	var running []*server
	for _, s := range m.servers {
		if strings.HasPrefix(wd+string(filepath.Separator), s.root+string(filepath.Separator)) ||
			strings.HasPrefix(s.root, wd+string(filepath.Separator)) {
			running = append(running, s)
		}
	}
	m.mu.Unlock()
	if len(running) > 0 {
		for _, s := range running {
			<-s.ready
		}
		return running, nil
	}
	var out []*server
	for _, l := range languages {
		for _, mk := range l.markers {
			if _, err := os.Stat(filepath.Join(wd, mk)); err != nil {
				continue
			}
			// serverFor keys by a file's directory; any file name will do.
			var ext string
			for e := range l.exts {
				ext = e
				break
			}
			if s, err := m.serverFor(filepath.Join(wd, "_"+ext)); err == nil && s != nil {
				out = append(out, s)
			}
			break
		}
	}
	return out, nil
}

var severityNames = []string{"", "error", "warning", "info", "hint"}

func formatDiagnostics(path string, ds []diagnostic, minSeverity int) (string, int) {
	var b strings.Builder
	n := 0
	for _, d := range ds {
		sev := cmp.Or(d.Severity, 1)
		if sev > minSeverity {
			continue
		}
		if n < 30 {
			src := ""
			if d.Source != "" {
				src = " (" + d.Source + ")"
			}
			fmt.Fprintf(&b, "  %s:%d:%d %s: %s%s\n", display(path), d.Range.Start.Line+1, d.Range.Start.Character+1,
				severityNames[min(sev, 4)], strings.ReplaceAll(strings.TrimSpace(d.Message), "\n", " "), src)
		}
		n++
	}
	if n > 30 {
		fmt.Fprintf(&b, "  … %d more\n", n-30)
	}
	return strings.TrimRight(b.String(), "\n"), n
}

func (m *Manager) diagnosticsTool(path string) (string, error) {
	s, abs, err := m.need(path)
	if err != nil {
		return "", err
	}
	ds, fresh, err := s.diagnostics(abs, diagWait)
	if err != nil {
		return "", err
	}
	if !fresh {
		return fmt.Sprintf("(no diagnostics from %s yet%s)", s.lang.name, s.indexingNote()), nil
	}
	body, n := formatDiagnostics(abs, ds, 2)
	if n == 0 {
		return display(abs) + ": no errors or warnings", nil
	}
	return fmt.Sprintf("%s: %d errors/warnings\n%s", display(abs), n, body), nil
}

// AfterEdit is the tools row's hook: the edited file's errors, or ""
// when no server takes the file.
func (m *Manager) AfterEdit(path string) string {
	abs, err := absPath(path)
	if err != nil {
		return ""
	}
	s, err := m.serverFor(abs)
	if err != nil || s == nil {
		return ""
	}
	ds, fresh, err := s.diagnostics(abs, diagWait)
	if err != nil {
		return ""
	}
	if !fresh {
		return fmt.Sprintf("[lsp] %s gave no diagnostics yet%s", s.lang.name, s.indexingNote())
	}
	body, n := formatDiagnostics(abs, ds, 1)
	if n == 0 {
		return "[lsp] no errors in " + display(abs)
	}
	return fmt.Sprintf("[lsp] %d errors in %s — fix the ones this edit caused:\n%s", n, display(abs), body)
}

var (
	grepCmd    = regexp.MustCompile(`(^|[\s;&|(])(rg|grep|git\s+grep|ag|ack)\s`)
	symbolLike = regexp.MustCompile(`^(?:def |class |fn |func |struct |impl |interface |type )?([A-Za-z_][A-Za-z0-9_]{3,})$`)
	argToken   = regexp.MustCompile(`'([^']*)'|"([^"]*)"|(\S+)`)
)

// BashNote is the tools row's bash hook: when a grep looks for a symbol
// in a project a language server covers, say that lsp would be exact.
// Once per symbol, a handful per session.
func (m *Manager) BashNote(cmd string) string {
	loc := grepCmd.FindStringIndex(cmd)
	if loc == nil {
		return ""
	}
	// The pattern is the first non-flag argument after the grep word,
	// up to the next pipe or separator.
	rest := cmd[loc[1]:]
	if i := strings.IndexAny(rest, "|;&\n"); i >= 0 {
		rest = rest[:i]
	}
	sym := ""
	for _, t := range argToken.FindAllStringSubmatch(rest, -1) {
		tok := t[1] + t[2] + t[3]
		if strings.HasPrefix(tok, "-") {
			continue
		}
		if mm := symbolLike.FindStringSubmatch(tok); mm != nil && (strings.Contains(mm[1], "_") || hasInnerUpper(mm[1]) || mm[0] != mm[1]) {
			sym = mm[1]
		}
		break
	}
	if sym == "" {
		return ""
	}
	m.mu.Lock()
	if m.nudged[sym] || len(m.nudged) >= 5 {
		m.mu.Unlock()
		return ""
	}
	m.mu.Unlock()
	if !m.covers() {
		return ""
	}
	m.mu.Lock()
	m.nudged[sym] = true
	m.mu.Unlock()
	return fmt.Sprintf("[lsp] %s looks like a symbol: tools.lsp.def(file, %q) and tools.lsp.refs(file, %q) find its definition and uses exactly (grep also matches comments, strings and look-alikes).", sym, sym, sym)
}

func hasInnerUpper(s string) bool {
	return strings.IndexFunc(s[1:], func(r rune) bool { return r >= 'A' && r <= 'Z' }) >= 0 &&
		strings.IndexFunc(s, func(r rune) bool { return r >= 'a' && r <= 'z' }) >= 0
}

// covers: a server is running, or one is installed for a language whose
// markers are in the working directory.
func (m *Manager) covers() bool {
	m.mu.Lock()
	running := len(m.servers) > 0
	m.mu.Unlock()
	if running {
		return true
	}
	wd, err := os.Getwd()
	if err != nil {
		return false
	}
	for _, l := range languages {
		for _, mk := range l.markers {
			if _, err := os.Stat(filepath.Join(wd, mk)); err == nil && l.command() != nil {
				return true
			}
		}
	}
	return false
}

// PromptSection steers the agent to the servers before grep.
const PromptSection = `Code intelligence — tools.lsp, real language servers (Python, Rust, TypeScript/JavaScript, Go, when installed):
- tools.lsp.def(path, symbol, [line]) -> where symbol, as written in path (on or near line), is defined, with the source line.
- tools.lsp.refs(path, symbol, [line]) -> every use of it across the project.
- tools.lsp.hover(path, symbol, [line]) -> its type, signature and docs.
- tools.lsp.outline(path) -> the file's classes and functions with their line ranges.
- tools.lsp.symbols(query, [path]) -> project symbols whose name matches query.
- tools.lsp.diagnostics(path) -> the file's type errors and warnings.
For a NAMED thing in code — a function, class, method, type, constant — use these before grep: they follow imports, and skip comments, strings and look-alike names. Keep grep for text that is not a symbol (log lines, config values, prose). Read a large file as outline, then tools.view(path, start, end) on the ranges you need; never page through it with sed -n or cat. tools.patch and tools.write end with the file's errors ("[lsp] …"): fix the ones your edit caused before moving on.`

// NativePromptSection is PromptSection for the engine: one native lsp
// tool with an op, not six tools.lsp functions.
const NativePromptSection = `Code intelligence — the lsp tool, real language servers (Python, Rust, TypeScript/JavaScript, Go, when installed): op is one of
- def, refs, hover (path, symbol, [line]) -> where symbol, as written in path (on or near line), is defined; every use of it across the project; its type, signature and docs.
- outline (path) -> the file's classes and functions with their line ranges.
- symbols (query, [path]) -> project symbols whose name matches query.
- diagnostics (path) -> the file's type errors and warnings.
For a NAMED thing in code — a function, class, method, type, constant — use these before grep: they follow imports, and skip comments, strings and look-alike names. Keep grep for text that is not a symbol (log lines, config values, prose). Read a large file as outline, then view the range you need.`

// promptFor is the section for the engine the session runs on: the
// "engine" key is provided only by engine-unreal.
func promptFor(ctx *kernel.Context) string {
	if _, err := kernel.Get[any](ctx, "engine"); err == nil {
		return NativePromptSection
	}
	return PromptSection
}

// statsHooks is the slice of the tools row's "turn-stats" service used.
type statsHooks interface {
	SetAfterEdit(fn func(path string) string)
	SetBashNote(fn func(cmd string) string)
}

type toolRegistry interface{ RegisterTool(name string, fn any) }

type sections interface{ Set(name, text string) }

type plugin struct{}

func init() {
	kernel.Register("lsp", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "lsp" }
func (plugin) Inject() []string { return []string{"codemode", "turn-stats"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		return fmt.Errorf("lsp: unknown config key %q", k)
	}
	reg, err := kernel.Get[toolRegistry](ctx, "codemode")
	if err != nil {
		return err
	}
	m := newManager()
	ctx.Effect(m.close)
	reg.RegisterTool("lsp", map[string]any{
		"def":         m.def,
		"refs":        m.refs,
		"hover":       m.hover,
		"outline":     m.outline,
		"symbols":     m.symbols,
		"diagnostics": m.diagnosticsTool,
	})
	ctx.Effect(func() { reg.RegisterTool("lsp", nil) })
	if at, err := kernel.Get[agenttools.Registry](ctx, "agent-tools"); err == nil {
		off, err := at.Register(m.nativeTool())
		if err != nil {
			return fmt.Errorf("lsp: %w", err)
		}
		ctx.Effect(off)
	}
	if d, ok := reg.(interface{ Describe(name, line string) }); ok {
		d.Describe("lsp", `tools.lsp.def|refs|hover(path, symbol, [line]), tools.lsp.outline(path), tools.lsp.symbols(query), tools.lsp.diagnostics(path) -> string: language-server navigation; see "Code intelligence".`)
	}
	if st, err := kernel.Get[statsHooks](ctx, "turn-stats"); err == nil {
		st.SetAfterEdit(m.AfterEdit)
		st.SetBashNote(m.BashNote)
		ctx.Effect(func() { st.SetAfterEdit(nil); st.SetBashNote(nil) })
	}
	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		s.Set("lsp", promptFor(ctx))
		ctx.Effect(func() { s.Set("lsp", "") })
	}
	ctx.Provide("lsp", m)
	return nil
}
