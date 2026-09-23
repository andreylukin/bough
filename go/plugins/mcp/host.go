package mcp

import (
	"context"
	"encoding/json"
	"fmt"
	"maps"
	"regexp"
	"slices"
	"sort"
	"strings"
	"sync"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"
)

// The programmatic surface. A model that reaches MCP through the shell
// pays a server start per call, sees no schema, and gets text back to
// parse; in practice it spent five discovery commands per real call.
// The host is one MCP client the row owns: a session per server kept
// open across calls, a catalog that holds each tool's schema, a search
// that returns callable signatures, and calls that return values.
// Code mode binds it as tools.mcp (bind.go); the engine sees it as
// three native tools (trio.go); native_tools' per-tool registrations
// (native.go) share its sessions.

// Envelope is what every call returns. ok is false only for a tool
// that reported isError; a transport failure, a policy refusal or a
// timeout is a Go error (a JS exception), since code cannot retry
// those the same way.
type Envelope struct {
	OK      bool             `json:"ok"`
	Value   any              `json:"value"`             // structuredContent, else the text (parsed as JSON when it is)
	Content []map[string]any `json:"content,omitempty"` // every content block, as the server sent it
	Text    string           `json:"text,omitempty"`    // the text blocks joined, for a server without structured output
	IsError bool             `json:"isError"`
	Error   *CallError       `json:"error,omitempty"`
	Meta    map[string]any   `json:"meta,omitempty"`
}

type CallError struct {
	Code    string `json:"code"` // "tool": the tool said so
	Message string `json:"message"`
}

// Hit is one search result: enough to call without another lookup.
type Hit struct {
	Server      string   `json:"server"`
	Tool        string   `json:"tool"`
	Description string   `json:"description"`
	Signature   string   `json:"signature"` // "tools.mcp.github.issueSearch({query: string, limit?: number})"
	Required    []string `json:"required,omitempty"`
	Score       float64  `json:"-"`
}

// Description is a tool in full: schema and a JSDoc block code can
// read before calling.
type Description struct {
	Server       string         `json:"server"`
	Tool         string         `json:"tool"`
	Description  string         `json:"description"`
	Signature    string         `json:"signature"`
	InputSchema  map[string]any `json:"inputSchema,omitempty"`
	OutputSchema map[string]any `json:"outputSchema,omitempty"`
	JSDoc        string         `json:"jsdoc"`
}

// Host is the row's MCP client.
type Host struct {
	servers map[string]ServerConfig
	connect func(ServerConfig) (*sdk.ClientSession, error)

	mu       sync.Mutex
	sessions map[string]*sdk.ClientSession
	cat      catalog
	listed   map[string]bool // servers listed in this process: the cache is refreshed once per server
	closed   bool
	// onBind is told of a tool the catalog learned after mount, so code
	// mode can add its stub live (bind.go).
	onBind func(server, tool string)
}

func newHost(servers map[string]ServerConfig, cat catalog) *Host {
	if cat.Servers == nil {
		cat.Servers = map[string][]catalogTool{}
	}
	return &Host{servers: servers, connect: connect, sessions: map[string]*sdk.ClientSession{}, cat: cat, listed: map[string]bool{}}
}

// Servers is the configured server names, sorted.
func (h *Host) Servers() []string { return slices.Sorted(maps.Keys(h.servers)) }

// Catalog is the tools known per server, from the cache or a listing.
func (h *Host) Catalog() map[string][]catalogTool {
	h.mu.Lock()
	defer h.mu.Unlock()
	out := map[string][]catalogTool{}
	for k, v := range h.cat.Servers {
		out[k] = slices.Clone(v)
	}
	return out
}

// session is the server's open session, connecting on first use. The
// connect runs unlocked: it can take seconds, and neither a call to
// another server nor the row's unmount should wait on it.
func (h *Host) session(server string) (*sdk.ClientSession, error) {
	sc, ok := h.servers[server]
	if !ok {
		return nil, fmt.Errorf("mcp: no server %q configured (servers: %s)", server, strings.Join(h.Servers(), ", "))
	}
	h.mu.Lock()
	s, closed := h.sessions[server], h.closed
	h.mu.Unlock()
	if closed {
		return nil, fmt.Errorf("mcp: the mcp row was unmounted")
	}
	if s != nil {
		return s, nil
	}
	s, err := h.connect(sc)
	if err != nil {
		return nil, fmt.Errorf("mcp: %s: connect: %w", server, err)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		s.Close()
		return nil, fmt.Errorf("mcp: the mcp row was unmounted")
	}
	if had := h.sessions[server]; had != nil {
		s.Close() // a concurrent call connected first; keep one session
		return had, nil
	}
	h.sessions[server] = s
	return s, nil
}

// forget drops a session a call failed on, so the next call reconnects:
// a stdio server that exited must not fail every later call.
func (h *Host) forget(server string, s *sdk.ClientSession) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.sessions[server] == s {
		delete(h.sessions, server)
		s.Close()
	}
}

// Close closes every session; later calls fail.
func (h *Host) Close() {
	h.mu.Lock()
	h.closed = true
	sessions := h.sessions
	h.sessions = map[string]*sdk.ClientSession{}
	h.mu.Unlock()
	for _, s := range sessions {
		s.Close()
	}
}

// Refresh lists the server's tools into the catalog (and the cache
// file), telling onBind about each. It is what search and describe do
// for a server never listed.
func (h *Host) Refresh(server string) error {
	s, err := h.session(server)
	if err != nil {
		return err
	}
	tools, err := listTools(s)
	if err != nil {
		h.forget(server, s)
		return fmt.Errorf("mcp: %s: list tools: %w", server, err)
	}
	h.mu.Lock()
	h.cat.Servers[server] = tools
	h.listed[server] = true
	cat := h.cat
	h.cat.At = now()
	bind := h.onBind
	h.mu.Unlock()
	_ = saveCatalog(cat)
	if bind != nil {
		for _, t := range tools {
			bind(server, t.Name)
		}
	}
	return nil
}

// ensure lists each configured server once per process, on the first
// search: a fresh install then sees real tools, and a cache written
// before schemas were kept (or by an older server) is brought up to
// date. A server that fails to answer is not retried until the next
// session, so one dead server costs one search its connect timeout.
func (h *Host) ensure() {
	for _, n := range h.Servers() {
		if h.servers[n].Disabled {
			continue
		}
		h.mu.Lock()
		done := h.listed[n]
		h.listed[n] = true
		h.mu.Unlock()
		if !done {
			_ = h.Refresh(n)
		}
	}
}

func (h *Host) lookup(server, tool string) (catalogTool, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, t := range h.cat.Servers[server] {
		if t.Name == tool {
			return t, true
		}
	}
	return catalogTool{}, false
}

var wordRe = regexp.MustCompile(`[A-Za-z0-9]+`)

func terms(s string) []string {
	var out []string
	for _, w := range wordRe.FindAllString(strings.ToLower(s), -1) {
		if len(w) > 1 {
			out = append(out, w)
		}
	}
	return out
}

// Search ranks tools for a query over server, name, description and
// parameter names and descriptions: a name match counts most, a
// parameter name next, prose least. limit 0 means 8.
func (h *Host) Search(query string, limit int) []Hit {
	if limit <= 0 {
		limit = 8
	}
	h.ensure()
	q := terms(query)
	if len(q) == 0 {
		return nil
	}
	h.mu.Lock()
	cat := h.cat.Servers
	h.mu.Unlock()
	var hits []Hit
	for server, tools := range cat {
		for _, t := range tools {
			name := terms(server + " " + t.Name)
			params, pdesc := paramTerms(t.Schema)
			prose := terms(t.Desc + " " + t.Full)
			score := 0.0
			for _, w := range q {
				switch {
				case slices.Contains(name, w):
					score += 3
				case slices.Contains(params, w):
					score += 2
				case slices.Contains(pdesc, w):
					score += 1
				case slices.Contains(prose, w):
					score += 1
				case prefixIn(name, w):
					score += 1.5
				}
			}
			if score == 0 {
				continue
			}
			sig, req := signature(server, t.Name, t.Schema)
			hits = append(hits, Hit{Server: server, Tool: t.Name, Description: t.Desc, Signature: sig, Required: req, Score: score})
		}
	}
	sort.SliceStable(hits, func(i, j int) bool {
		if hits[i].Score != hits[j].Score {
			return hits[i].Score > hits[j].Score
		}
		return hits[i].Server+hits[i].Tool < hits[j].Server+hits[j].Tool
	})
	if len(hits) > limit {
		hits = hits[:limit]
	}
	return hits
}

func prefixIn(words []string, w string) bool {
	for _, x := range words {
		if strings.HasPrefix(x, w) || strings.HasPrefix(w, x) {
			return true
		}
	}
	return false
}

func paramTerms(schema map[string]any) (names, descs []string) {
	props, _ := schema["properties"].(map[string]any)
	for k, v := range props {
		names = append(names, terms(k)...)
		if m, ok := v.(map[string]any); ok {
			if d, ok := m["description"].(string); ok {
				descs = append(descs, terms(d)...)
			}
		}
	}
	return names, descs
}

// Describe is the tool in full, listing its server first when the
// catalog does not know it.
func (h *Host) Describe(server, tool string) (Description, error) {
	if _, ok := h.servers[server]; !ok {
		return Description{}, fmt.Errorf("mcp: no server %q configured (servers: %s)", server, strings.Join(h.Servers(), ", "))
	}
	t, ok := h.lookup(server, tool)
	if !ok {
		if err := h.Refresh(server); err != nil {
			return Description{}, err
		}
		if t, ok = h.lookup(server, tool); !ok {
			return Description{}, fmt.Errorf("mcp: %s has no tool %q (tools.mcp.search finds one)", server, tool)
		}
	}
	sig, _ := signature(server, t.Name, t.Schema)
	desc := t.Full
	if desc == "" {
		desc = t.Desc
	}
	return Description{Server: server, Tool: t.Name, Description: desc, Signature: sig, InputSchema: t.Schema, OutputSchema: t.Output, JSDoc: jsdoc(server, t)}, nil
}

// Call runs one tool and returns its result as a value. args is the
// argument object; nil is {}. The catalog learns a tool it did not
// know, so a call is never refused for a stale cache.
func (h *Host) Call(ctx context.Context, server, tool string, args map[string]any) (Envelope, error) {
	if args == nil {
		args = map[string]any{}
	}
	if _, ok := h.lookup(server, tool); !ok {
		if _, err := h.Describe(server, tool); err != nil {
			return Envelope{}, err
		}
	}
	s, err := h.session(server)
	if err != nil {
		return Envelope{}, err
	}
	if _, has := ctx.Deadline(); !has {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, callTimeout)
		defer cancel()
	}
	res, err := s.CallTool(ctx, &sdk.CallToolParams{Name: tool, Arguments: args})
	if err != nil {
		if ctx.Err() == nil {
			h.forget(server, s)
		}
		return Envelope{}, fmt.Errorf("mcp: %s/%s: %w", server, tool, err)
	}
	return envelope(server, tool, res), nil
}

// envelope maps a result to what code sees: structured content as the
// value when the server sent it, else the text (parsed when it is
// JSON), every content block kept beside it.
func envelope(server, tool string, res *sdk.CallToolResult) Envelope {
	e := Envelope{OK: !res.IsError, IsError: res.IsError}
	var text strings.Builder
	for _, c := range res.Content {
		if tc, ok := c.(*sdk.TextContent); ok {
			text.WriteString(tc.Text)
		}
		if raw, err := json.Marshal(c); err == nil {
			var m map[string]any
			if json.Unmarshal(raw, &m) == nil {
				e.Content = append(e.Content, m)
			}
		}
	}
	e.Text = text.String()
	if res.StructuredContent != nil {
		e.Value = plain(res.StructuredContent)
	} else if t := strings.TrimSpace(e.Text); strings.HasPrefix(t, "{") || strings.HasPrefix(t, "[") {
		var v any
		if json.Unmarshal([]byte(t), &v) == nil {
			e.Value = v
		}
	}
	if e.Value == nil && !res.IsError {
		e.Value = e.Text
	}
	if len(res.Meta) > 0 {
		e.Meta = res.Meta
	}
	if res.IsError {
		msg := strings.TrimSpace(e.Text)
		if msg == "" {
			msg = server + "/" + tool + " failed"
		}
		e.Error = &CallError{Code: "tool", Message: msg}
		e.Value = nil
	}
	return e
}

// plain round-trips through JSON so goja sees maps and slices, never
// SDK structs.
func plain(v any) any {
	raw, err := json.Marshal(v)
	if err != nil {
		return v
	}
	var out any
	if json.Unmarshal(raw, &out) != nil {
		return v
	}
	return out
}

// signature is the call as code writes it: server and tool as JS
// identifiers, the parameters typed from the schema, optional ones
// marked, required ones returned.
func signature(server, tool string, schema map[string]any) (string, []string) {
	props, _ := schema["properties"].(map[string]any)
	var req []string
	if r, ok := schema["required"].([]any); ok {
		for _, x := range r {
			if s, ok := x.(string); ok {
				req = append(req, s)
			}
		}
	}
	keys := slices.Sorted(maps.Keys(props))
	// Required first, as a reader expects.
	sort.SliceStable(keys, func(i, j int) bool {
		ri, rj := slices.Contains(req, keys[i]), slices.Contains(req, keys[j])
		return ri && !rj
	})
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		opt := "?"
		if slices.Contains(req, k) {
			opt = ""
		}
		parts = append(parts, k+opt+": "+jsType(props[k]))
	}
	return fmt.Sprintf("tools.mcp.%s.%s({%s})", ident(server), ident(tool), strings.Join(parts, ", ")), req
}

// jsType is a JSON Schema fragment as a TypeScript-ish type, one level
// deep: enough to write the call, with the schema there for the rest.
func jsType(v any) string {
	m, ok := v.(map[string]any)
	if !ok {
		return "any"
	}
	if e, ok := m["enum"].([]any); ok && len(e) > 0 {
		parts := make([]string, 0, len(e))
		for _, x := range e {
			b, _ := json.Marshal(x)
			parts = append(parts, string(b))
		}
		return strings.Join(parts, " | ")
	}
	switch t := m["type"].(type) {
	case string:
		switch t {
		case "array":
			return jsType(m["items"]) + "[]"
		case "integer":
			return "number"
		case "object":
			if props, ok := m["properties"].(map[string]any); ok && len(props) > 0 {
				keys := slices.Sorted(maps.Keys(props))
				parts := make([]string, 0, len(keys))
				for _, k := range keys {
					parts = append(parts, k+": "+jsType(props[k]))
				}
				return "{" + strings.Join(parts, ", ") + "}"
			}
			return "object"
		}
		return t
	case []any:
		parts := make([]string, 0, len(t))
		for _, x := range t {
			if s, ok := x.(string); ok {
				parts = append(parts, s)
			}
		}
		return strings.Join(parts, " | ")
	}
	if _, ok := m["oneOf"]; ok {
		return "any"
	}
	return "any"
}

// jsdoc is the tool as a documented function, the way a generated
// binding would read.
func jsdoc(server string, t catalogTool) string {
	sig, _ := signature(server, t.Name, t.Schema)
	var b strings.Builder
	b.WriteString("/**\n")
	desc := t.Full
	if desc == "" {
		desc = t.Desc
	}
	for _, ln := range strings.Split(strings.TrimSpace(desc), "\n") {
		b.WriteString(" * " + ln + "\n")
	}
	props, _ := t.Schema["properties"].(map[string]any)
	if len(props) > 0 {
		b.WriteString(" * @param {Object} args\n")
		for _, k := range slices.Sorted(maps.Keys(props)) {
			d := ""
			if m, ok := props[k].(map[string]any); ok {
				d, _ = m["description"].(string)
			}
			b.WriteString(fmt.Sprintf(" * @param {%s} args.%s %s\n", jsType(props[k]), k, strings.TrimSpace(d)))
		}
	}
	ret := "{ok: boolean, value: any, text: string, content: object[], isError: boolean, error?: {code, message}}"
	if t.Output != nil {
		ret = "{ok: boolean, value: " + jsType(t.Output) + ", ...}"
	}
	b.WriteString(" * @returns " + ret + "\n */\n")
	b.WriteString(sig)
	return b.String()
}

// ident is a name as a JS property: "issue-search" reads as
// issueSearch; a name that is already an identifier is unchanged.
// tools.mcp["issue-search"] works too (bind.go sets both).
func ident(s string) string {
	if isIdent(s) {
		return s
	}
	var b strings.Builder
	up := false
	for i, r := range s {
		switch {
		case r == '_' || r == '-' || r == '.' || r == '/' || r == ' ':
			up = i > 0 && b.Len() > 0
		case up:
			b.WriteString(strings.ToUpper(string(r)))
			up = false
		default:
			b.WriteRune(r)
		}
	}
	out := b.String()
	if out == "" || (out[0] >= '0' && out[0] <= '9') {
		out = "_" + out
	}
	return out
}

func isIdent(s string) bool {
	if s == "" {
		return false
	}
	for i, r := range s {
		ok := r == '_' || r == '$' || (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (i > 0 && r >= '0' && r <= '9')
		if !ok {
			return false
		}
	}
	return true
}
