// Package artifacts is the artifacts plugin: pages an agent publishes
// for a person to read in the browser — a comparison, a report, a
// dashboard — hosted by bough itself.
//
// The model does not write HTML. It writes a small JSON spec (a title
// and a list of typed blocks: text, stats, table, chart, ...); this
// row checks the spec and says exactly what is wrong, stores it under
// ~/.bough/artifacts/<session>/<name>.json, and serves it through the
// web row, where a bundled viewer draws it. The look — type, colour,
// light and dark, sortable tables, charts — lives in the viewer once,
// so the model spends its tokens on content, and every page reads as
// one system.
//
// The filesystem is the source of truth: whichever bough process holds
// the web row's port serves every session's pages.
package artifacts

import (
	_ "embed"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/web"
)

//go:embed viewer.html
var viewerHTML string

//go:embed lib/echarts.min.js
var echartsJS []byte

// registry is the slice of codemode this plugin needs.
type registry interface {
	RegisterTool(name string, fn any)
}

type describer interface{ Describe(name, line string) }
type sections interface{ Set(name, text string) }
type pather interface{ Path() string }

// Store is the directory of published pages and the server they are
// read from.
type Store struct {
	root    string // ~/.bough/artifacts
	session string // this session's directory name
	web     *web.Service
	open    func(string) error // hands a URL to the browser; nil = never

	mu     sync.Mutex
	opened map[string]bool // names this process has opened once
	last   string          // the URL published most recently
}

var nameRe = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]{0,63}$`)

// cleanName makes a URL-safe page name from what the model passed.
func cleanName(name string) (string, error) {
	n := strings.ToLower(strings.TrimSpace(name))
	n = strings.TrimSuffix(strings.TrimSuffix(n, ".json"), ".html")
	n = strings.NewReplacer(" ", "-", "/", "-", "\\", "-").Replace(n)
	if !nameRe.MatchString(n) || strings.Contains(n, "..") {
		return "", fmt.Errorf("artifact name %q: use letters, digits, - and _ (e.g. \"db-comparison\")", name)
	}
	return n, nil
}

// Publish validates spec and writes it as name under this session,
// returning the page's URL. A name published twice is replaced: the
// page reloads, its URL stays.
func (s *Store) Publish(name string, spec any) (string, error) {
	n, err := cleanName(name)
	if err != nil {
		return "", err
	}
	page, err := Normalize(spec)
	if err != nil {
		return "", err
	}
	page["published"] = time.Now().UTC().Format(time.RFC3339)
	dir := filepath.Join(s.root, s.session)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("artifact: %w", err)
	}
	b, err := json.MarshalIndent(page, "", " ")
	if err != nil {
		return "", fmt.Errorf("artifact: %w", err)
	}
	if err := os.WriteFile(filepath.Join(dir, n+".json"), b, 0o644); err != nil {
		return "", fmt.Errorf("artifact: %w", err)
	}
	url := s.web.URL() + "/artifacts/" + s.session + "/" + n
	s.mu.Lock()
	first := !s.opened[n]
	s.opened[n] = true
	s.last = url
	s.mu.Unlock()
	if first && s.open != nil {
		_ = s.open(url)
	}
	return url, nil
}

// entry is one published page as the index lists it.
type entry struct {
	Session, Name, Title, Published string
}

// list is every page under root, newest first; session narrows it.
func (s *Store) list(session string) []entry {
	var out []entry
	sessions := []string{session}
	if session == "" {
		ds, _ := os.ReadDir(s.root)
		sessions = sessions[:0]
		for _, d := range ds {
			if d.IsDir() {
				sessions = append(sessions, d.Name())
			}
		}
	}
	for _, sess := range sessions {
		files, _ := os.ReadDir(filepath.Join(s.root, sess))
		for _, f := range files {
			if !strings.HasSuffix(f.Name(), ".json") {
				continue
			}
			e := entry{Session: sess, Name: strings.TrimSuffix(f.Name(), ".json")}
			if b, err := os.ReadFile(filepath.Join(s.root, sess, f.Name())); err == nil {
				var p struct{ Title, Published string }
				_ = json.Unmarshal(b, &p)
				e.Title, e.Published = p.Title, p.Published
			}
			out = append(out, e)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Published > out[j].Published })
	return out
}

// List is the /artifacts command: this session's pages with their URLs.
func (s *Store) List() string {
	es := s.list(s.session)
	if len(es) == 0 {
		return "no artifacts published in this session yet (the agent publishes one with tools.artifact); all sessions: " + s.web.URL() + "/artifacts/"
	}
	var b strings.Builder
	for _, e := range es {
		fmt.Fprintf(&b, "%s  %s/artifacts/%s/%s\n", e.Title, s.web.URL(), e.Session, e.Name)
	}
	fmt.Fprintf(&b, "all sessions: %s/artifacts/", s.web.URL())
	return b.String()
}

// ServeHTTP serves /artifacts/ (the index), /artifacts/_lib/*, and
// /artifacts/<session>/<name>[.json].
func (s *Store) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	rest := strings.TrimPrefix(path.Clean(r.URL.Path), "/artifacts")
	rest = strings.TrimPrefix(rest, "/")
	switch {
	case rest == "":
		s.serveIndex(w)
	case rest == "_lib/echarts.min.js":
		w.Header().Set("Content-Type", "application/javascript")
		w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		_, _ = w.Write(echartsJS)
	default:
		sess, name, ok := strings.Cut(rest, "/")
		if !ok || strings.Contains(name, "/") || strings.Contains(sess, "..") || strings.Contains(name, "..") {
			http.NotFound(w, r)
			return
		}
		raw := strings.HasSuffix(name, ".json")
		name = strings.TrimSuffix(name, ".json")
		b, err := os.ReadFile(filepath.Join(s.root, sess, name+".json"))
		if err != nil {
			http.NotFound(w, r)
			return
		}
		if raw {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write(b)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write([]byte(render(b)))
	}
}

// render is the viewer with the spec inlined, so a page is one request.
func render(spec []byte) string {
	safe := strings.ReplaceAll(string(spec), "</", `<\/`)
	return strings.Replace(viewerHTML, "/*SPEC*/null", safe, 1)
}

func (s *Store) serveIndex(w http.ResponseWriter) {
	es := s.list("")
	var b strings.Builder
	b.WriteString(`<!doctype html><meta charset="utf-8"><title>artifacts</title><style>body{font:15px/1.5 system-ui,sans-serif;max-width:70ch;margin:3rem auto;padding:0 1rem;color:#1a1a1a;background:#fafaf7}@media(prefers-color-scheme:dark){body{color:#e6e4dd;background:#15161a}a{color:#9fc0ff}}h1{font-size:1rem;letter-spacing:.08em;text-transform:uppercase;font-family:ui-monospace,monospace}li{margin:.4rem 0}small{opacity:.6;font-family:ui-monospace,monospace;font-size:.8rem}</style><h1>artifacts</h1>`)
	if len(es) == 0 {
		b.WriteString("<p>nothing published yet.</p>")
	}
	b.WriteString("<ul>")
	for _, e := range es {
		fmt.Fprintf(&b, `<li><a href="/artifacts/%s/%s">%s</a> <small>%s · %s</small></li>`, e.Session, e.Name, html(e.Title), html(e.Session), html(e.Published))
	}
	b.WriteString("</ul>")
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_, _ = w.Write([]byte(b.String()))
}

func html(s string) string {
	return strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;").Replace(s)
}

// PromptSection tells the model when a page earns its place and how to
// write one; the spec guide follows it.
const PromptSection = `Artifacts — pages you publish for the user to read in the browser: tools.artifact(name, spec) -> URL.
Make one when the user will scan, compare, keep or interact with the result: a comparison, a report, a dashboard, a plan. Not for a short answer, and not to dress prose up — reply in text then.
The spec is content only; the viewer owns the look (type, colour, light and dark, sortable tables, charts). Reply with the URL and one line on what the page shows; do not repeat the page in chat. A rejected spec comes back as a list of problems — fix them and publish again under the same name.
` + Guide

type plugin struct{}

func init() {
	kernel.Register("artifacts", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "artifacts" }
func (plugin) Inject() []string { return []string{"codemode", "web"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	s := &Store{opened: map[string]bool{}, open: web.Open}
	for k, v := range cfg {
		switch k {
		case "dir":
			s.root, _ = v.(string)
		case "open":
			if b, ok := v.(bool); ok && !b {
				s.open = nil
			} else if str, ok := v.(string); ok && str == "false" {
				s.open = nil
			}
		default:
			return fmt.Errorf("artifacts: unknown config key %q", k)
		}
	}
	if s.root == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("artifacts: home dir: %w", err)
		}
		s.root = filepath.Join(home, ".bough", "artifacts")
	}
	s.session = "session"
	if h, err := kernel.Get[pather](ctx, "history"); err == nil && h.Path() != "" {
		s.session = strings.TrimSuffix(filepath.Base(h.Path()), filepath.Ext(h.Path()))
	}
	w, err := kernel.Get[*web.Service](ctx, "web")
	if err != nil {
		return fmt.Errorf("artifacts: needs the web row")
	}
	s.web = w
	w.Handle("/artifacts/", s)
	ctx.Effect(func() { w.Unhandle("/artifacts/") })

	code, err := kernel.Get[registry](ctx, "codemode")
	if err != nil {
		return err
	}
	code.RegisterTool("artifact", s.Publish)
	if d, ok := code.(describer); ok {
		d.Describe("artifact", `tools.artifact(name, spec) publishes a page for the user to read in the browser and returns its URL; spec = {title, blocks: [...]} (see Artifacts).`)
	}
	ctx.Effect(func() { code.RegisterTool("artifact", nil) })

	if sec, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		sec.Set("artifacts", PromptSection)
		ctx.Effect(func() { sec.Set("artifacts", "") })
	}
	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "artifacts", Usage: "[open]", Summary: "pages published this session, with their URLs; open = the latest in the browser"}
		run := func(args string) (string, error) {
			if args == "open" {
				s.mu.Lock()
				url := s.last
				s.mu.Unlock()
				if url == "" {
					url = s.web.URL() + "/artifacts/"
				}
				if err := web.Open(url); err != nil {
					return url, nil
				}
				return "opened " + url, nil
			}
			return s.List(), nil
		}
		if err := reg.Register(info, run); err != nil {
			return err
		}
		ctx.Effect(func() { reg.Unregister("artifacts") })
	}
	ctx.Provide("artifacts", s)
	return nil
}
