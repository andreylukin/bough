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
	"errors"
	"fmt"
	"net/http"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
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

//go:embed lib/mermaid.min.js
var mermaidJS []byte

// registry is the slice of codemode this plugin needs.
type registry interface {
	RegisterTool(name string, fn any)
}

type describer interface{ Describe(name, line string) }

// notifier is the loop's notices seam (tools-basic's jobs): an answer
// on a page lands like a finished job and wakes an idle agent.
type notifier interface{ Notify(text string) }
type sections interface{ Set(name, text string) }
type pather interface{ Path() string }

// Store is the directory of published pages and the server they are
// read from.
type Store struct {
	root    string // ~/.bough/artifacts
	session string // this session's directory name
	web     *web.Service
	open    func(string) error // hands a URL to the browser; nil = never

	notify func(string) // queues a notice for the agent; nil = none

	mu     sync.Mutex
	opened map[string]bool // names this process has opened once
	last   string          // the URL published most recently
	seen   map[string]int  // answers log length already noticed, by name
}

// Answers is what the user did on a page: the latest value per block
// id, the notes sent, and an append-only log the agent is told about.
type Answers struct {
	Answers map[string]any `json:"answers"`
	Notes   []Note         `json:"notes"`
	Log     []LogEntry     `json:"log"`
}

type Note struct {
	At   string `json:"at"`
	Text string `json:"text"`
}

type LogEntry struct {
	At    string `json:"at"`
	ID    string `json:"id,omitempty"`
	Kind  string `json:"kind"` // decision, checklist, form, note
	Value any    `json:"value"`
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
	if err := s.save(n, page); err != nil {
		return "", err
	}
	url := s.url(n)
	s.mu.Lock()
	first := !s.opened[n]
	s.opened[n] = true
	s.last = url
	if _, known := s.seen[n]; !known {
		// From here on every answer on this page is news.
		s.seen[n] = len(readAnswers(answersPath(s.root, s.session, n)).Log)
	}
	s.mu.Unlock()
	if first && s.open != nil {
		_ = s.open(url)
	}
	return url, nil
}

func (s *Store) url(name string) string {
	return s.web.URL() + "/artifacts/" + s.session + "/" + name
}

func (s *Store) specPath(name string) string {
	return filepath.Join(s.root, s.session, name+".json")
}

// save stamps the page and writes it; the version climbs so an open
// page knows to reload.
func (s *Store) save(name string, page map[string]any) error {
	version := 1
	if old, err := s.load(name); err == nil {
		if v, ok := old["version"].(float64); ok {
			version = int(v) + 1
		}
	}
	page["published"] = time.Now().UTC().Format(time.RFC3339)
	page["version"] = version
	if err := os.MkdirAll(filepath.Dir(s.specPath(name)), 0o755); err != nil {
		return fmt.Errorf("artifact: %w", err)
	}
	b, err := json.MarshalIndent(page, "", " ")
	if err != nil {
		return fmt.Errorf("artifact: %w", err)
	}
	if err := os.WriteFile(s.specPath(name), b, 0o644); err != nil {
		return fmt.Errorf("artifact: %w", err)
	}
	return nil
}

// load reads this session's stored page.
func (s *Store) load(name string) (map[string]any, error) {
	b, err := os.ReadFile(s.specPath(name))
	if err != nil {
		return nil, err
	}
	var page map[string]any
	if err := json.Unmarshal(b, &page); err != nil {
		return nil, err
	}
	return page, nil
}

// Patch changes a published page by ops (see Apply) and returns its
// URL. The result is validated whole; a refused patch leaves the page
// as it was.
func (s *Store) Patch(name string, ops any) (string, error) {
	n, err := cleanName(name)
	if err != nil {
		return "", err
	}
	page, err := s.load(n)
	if err != nil {
		return "", fmt.Errorf("artifact %q is not published in this session (publish it first with tools.artifact)", n)
	}
	list, ok := ops.([]any)
	if !ok {
		return "", errors.New("ops must be an array of {op, path, value?}")
	}
	if err := Apply(page, list); err != nil {
		return "", fmt.Errorf("artifact patch: %w", err)
	}
	page, err = Normalize(page)
	if err != nil {
		return "", err
	}
	if err := s.save(n, page); err != nil {
		return "", err
	}
	return s.url(n), nil
}

func answersPath(root, session, name string) string {
	return filepath.Join(root, session, name+".answers.json")
}

func readAnswers(path string) Answers {
	a := Answers{Answers: map[string]any{}}
	if b, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(b, &a)
		if a.Answers == nil {
			a.Answers = map[string]any{}
		}
	}
	return a
}

// Answers is tools.artifactAnswers(name): what the user chose, ticked,
// filled in and wrote on the page, as JSON.
func (s *Store) Answers(name string) (string, error) {
	n, err := cleanName(name)
	if err != nil {
		return "", err
	}
	if _, err := os.Stat(s.specPath(n)); err != nil {
		return "", fmt.Errorf("artifact %q is not published in this session", n)
	}
	a := readAnswers(answersPath(s.root, s.session, n))
	s.mu.Lock()
	s.seen[n] = len(a.Log)
	s.mu.Unlock()
	if len(a.Log) == 0 {
		return "no answers yet on " + n, nil
	}
	b, _ := json.MarshalIndent(map[string]any{"answers": a.Answers, "notes": a.Notes}, "", " ")
	return string(b), nil
}

var answerMu sync.Mutex

// record appends one answer from the page and returns the new log
// length. Serialised across handlers: two tabs may post at once.
func record(path string, e LogEntry) (Answers, error) {
	answerMu.Lock()
	defer answerMu.Unlock()
	a := readAnswers(path)
	e.At = time.Now().UTC().Format(time.RFC3339)
	switch e.Kind {
	case "note":
		text, _ := e.Value.(string)
		if strings.TrimSpace(text) == "" {
			return a, errors.New("empty note")
		}
		a.Notes = append(a.Notes, Note{At: e.At, Text: text})
	default:
		if e.ID == "" {
			return a, errors.New("answer needs an id")
		}
		a.Answers[e.ID] = e.Value
	}
	a.Log = append(a.Log, e)
	b, err := json.MarshalIndent(a, "", " ")
	if err != nil {
		return a, err
	}
	return a, os.WriteFile(path, b, 0o644)
}

// watchAnswers tells the agent about answers as they arrive: every
// two seconds the session's answers files are read and any log entry
// not yet noticed becomes a notice. File-based on purpose — the
// process serving the page may not be the one running this session.
func (s *Store) watchAnswers(stop <-chan struct{}) {
	t := time.NewTicker(2 * time.Second)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
		}
		files, _ := filepath.Glob(filepath.Join(s.root, s.session, "*.answers.json"))
		for _, f := range files {
			name := strings.TrimSuffix(filepath.Base(f), ".answers.json")
			a := readAnswers(f)
			s.mu.Lock()
			seen, known := s.seen[name]
			if !known {
				// First sight: a resumed session does not replay old answers.
				seen = len(a.Log)
			}
			s.seen[name] = len(a.Log)
			s.mu.Unlock()
			for _, e := range a.Log[min(seen, len(a.Log)):] {
				if s.notify != nil {
					s.notify(noticeText(name, e))
				}
			}
		}
	}
}

// noticeText is one answer as the agent reads it.
func noticeText(name string, e LogEntry) string {
	v, _ := json.Marshal(e.Value)
	switch e.Kind {
	case "note":
		return fmt.Sprintf("[artifact %s] the user wrote: %s", name, e.Value)
	default:
		return fmt.Sprintf("[artifact %s] %s %s = %s (tools.artifactAnswers(%q) has everything)", name, e.Kind, e.ID, v, name)
	}
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
	case rest == "_lib/echarts.min.js" || rest == "_lib/mermaid.min.js":
		w.Header().Set("Content-Type", "application/javascript")
		w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		if strings.HasSuffix(rest, "mermaid.min.js") {
			_, _ = w.Write(mermaidJS)
		} else {
			_, _ = w.Write(echartsJS)
		}
	default:
		sess, name, ok := strings.Cut(rest, "/")
		if !ok || strings.Contains(sess, "..") || strings.Contains(name, "..") {
			http.NotFound(w, r)
			return
		}
		name, sub, _ := strings.Cut(name, "/")
		raw := strings.HasSuffix(name, ".json")
		name = strings.TrimSuffix(name, ".json")
		specFile := filepath.Join(s.root, sess, name+".json")
		b, err := os.ReadFile(specFile)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		switch sub {
		case "answers":
			s.serveAnswers(w, r, answersPath(s.root, sess, name))
			return
		case "events":
			serveEvents(w, r, specFile)
			return
		case "":
		default:
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

// serveAnswers reads (GET) or records (POST {id?, kind, value}) a
// page's answers.
func (s *Store) serveAnswers(w http.ResponseWriter, r *http.Request, path string) {
	w.Header().Set("Content-Type", "application/json")
	switch r.Method {
	case http.MethodGet:
		_ = json.NewEncoder(w).Encode(readAnswers(path))
	case http.MethodPost:
		var e LogEntry
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&e); err != nil {
			http.Error(w, `{"error":"bad json"}`, 400)
			return
		}
		a, err := record(path, e)
		if err != nil {
			http.Error(w, `{"error":`+strconv.Quote(err.Error())+`}`, 400)
			return
		}
		_ = json.NewEncoder(w).Encode(a)
	default:
		http.Error(w, "method", 405)
	}
}

// serveEvents is the page's live-reload stream: server-sent events,
// one "update" whenever the spec file changes on disk (a publish or a
// patch from any process), a comment every 15 s to keep it open.
func serveEvents(w http.ResponseWriter, r *http.Request, specFile string) {
	fl, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "no streaming", 500)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	last := mtime(specFile)
	fmt.Fprint(w, ": open\n\n")
	fl.Flush()
	t := time.NewTicker(time.Second)
	defer t.Stop()
	beat := 0
	for {
		select {
		case <-r.Context().Done():
			return
		case <-t.C:
		}
		if m := mtime(specFile); m != last {
			last = m
			fmt.Fprint(w, "event: update\ndata: {}\n\n")
			fl.Flush()
			continue
		}
		if beat++; beat%15 == 0 {
			fmt.Fprint(w, ": beat\n\n")
			fl.Flush()
		}
	}
}

func mtime(path string) int64 {
	fi, err := os.Stat(path)
	if err != nil {
		return 0
	}
	return fi.ModTime().UnixNano()
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
A page can ask: decision, checklist and form blocks (and the note box) send the user's answers back to you as notices while you work or wait; when you need the user to choose, publish the page and end your turn — the answer wakes you.
` + Guide

type plugin struct{}

func init() {
	kernel.Register("artifacts", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "artifacts" }
func (plugin) Inject() []string { return []string{"codemode", "web"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	s := &Store{opened: map[string]bool{}, seen: map[string]int{}, open: web.Open}
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
	code.RegisterTool("artifactPatch", s.Patch)
	code.RegisterTool("artifactAnswers", s.Answers)
	if d, ok := code.(describer); ok {
		d.Describe("artifact", `tools.artifact(name, spec) publishes a page for the user to read in the browser and returns its URL; spec = {title, data?, blocks: [...]} (see Artifacts).`)
		d.Describe("artifactPatch", `tools.artifactPatch(name, ops) changes a published page in place (ops: [{op: set|append|remove, path, value?}]); the open page reloads itself.`)
		d.Describe("artifactAnswers", `tools.artifactAnswers(name) -> JSON: what the user chose, ticked, filled in and wrote on the page.`)
	}
	ctx.Effect(func() {
		code.RegisterTool("artifact", nil)
		code.RegisterTool("artifactPatch", nil)
		code.RegisterTool("artifactAnswers", nil)
	})
	if n, err := kernel.Get[notifier](ctx, "job-notices"); err == nil {
		s.notify = n.Notify
	}
	stop := make(chan struct{})
	go s.watchAnswers(stop)
	ctx.Effect(func() { close(stop) })

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
