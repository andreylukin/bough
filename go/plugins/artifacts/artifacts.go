// Package artifacts is the artifacts plugin: pages an agent publishes
// for a person to read and act on in the browser — a comparison, a
// report, a dashboard, a form — hosted by bough itself.
//
// The model writes OpenUI Lang (thesysdev/openui): a compact,
// line-oriented language of component calls — root = Card([...]),
// tables, charts, forms, tabs, buttons, reactive $variables — and
// this row stores the program under ~/.bough/artifacts/<session>/
// <name>.ui and serves it through the web row, where the vendored
// OpenUI browser bundle (React + renderer + component library, MIT)
// draws it. The look is the library's; the model spends its tokens on
// content and structure.
//
// The page talks back: a button, a form submit or a follow-up posts
// an action to /artifacts/<session>/<name>/answers, and the note box
// a note; the agent is told through the loop's notices, so an idle
// agent wakes on it. The renderer's own structured errors (unknown
// component, missing argument) come back the same way, which is the
// repair loop: publish, read the notice, patch.
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

//go:embed lib/openui-bundle.min.js
var bundleJS []byte

//go:embed lib/openui-styles.css
var bundleCSS []byte

//go:embed lib/openui-prompt.md
var langGuide string

// registry is the slice of codemode this plugin needs.
type registry interface {
	RegisterTool(name string, fn any)
}

type describer interface{ Describe(name, line string) }

// notifier is the loop's notices seam (tools-basic's jobs): an answer
// or an error on a page lands like a finished job and wakes an idle
// agent.
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
	notify  func(string)       // queues a notice for the agent; nil = none

	mu      sync.Mutex
	opened  map[string]bool   // names this process has published
	last    string            // the URL published most recently
	seen    map[string]int    // answers log length already noticed, by name
	errSeen map[string]string // the errors report already noticed, by name
}

// Answers is what the user did on a page: the latest form state, the
// actions taken, the notes sent, and an append-only log the agent is
// told about.
type Answers struct {
	State map[string]any `json:"state"` // the page's form fields, as last seen
	Notes []Note         `json:"notes"`
	Log   []LogEntry     `json:"log"`
}

type Note struct {
	At   string `json:"at"`
	Text string `json:"text"`
}

// LogEntry is one thing the user did: kind "action" carries the
// renderer's ActionEvent (type, message, form state), "note" a note.
type LogEntry struct {
	At    string `json:"at"`
	Kind  string `json:"kind"`
	Value any    `json:"value"`
}

// Errors is what the renderer reported for a page version: the
// parser's structured errors, empty when it rendered clean.
type Errors struct {
	Version int              `json:"version"`
	Errors  []map[string]any `json:"errors"`
}

// meta sits beside a program: when it was published and its version,
// so an open page knows to reload and errors are tied to a version.
type meta struct {
	Published string `json:"published"`
	Version   int    `json:"version"`
}

var nameRe = regexp.MustCompile(`^[a-z0-9][a-z0-9._-]{0,63}$`)

// cleanName makes a URL-safe page name from what the model passed.
func cleanName(name string) (string, error) {
	n := strings.ToLower(strings.TrimSpace(name))
	n = strings.TrimSuffix(strings.TrimSuffix(n, ".ui"), ".html")
	n = strings.NewReplacer(" ", "-", "/", "-", "\\", "-").Replace(n)
	if !nameRe.MatchString(n) || strings.Contains(n, "..") {
		return "", fmt.Errorf("artifact name %q: use letters, digits, - and _ (e.g. \"db-comparison\")", name)
	}
	return n, nil
}

func (s *Store) url(name string) string {
	return s.web.URL() + "/artifacts/" + s.session + "/" + name
}

func (s *Store) codePath(name string) string {
	return filepath.Join(s.root, s.session, name+".ui")
}

func metaPath(codeFile string) string { return strings.TrimSuffix(codeFile, ".ui") + ".meta.json" }
func answersPath(codeFile string) string {
	return strings.TrimSuffix(codeFile, ".ui") + ".answers.json"
}
func errorsPath(codeFile string) string { return strings.TrimSuffix(codeFile, ".ui") + ".errors.json" }

// Publish checks code and writes it as name under this session,
// returning the page's URL. A name published twice is replaced: the
// page reloads, its URL stays.
func (s *Store) Publish(name string, code string) (string, error) {
	n, err := cleanName(name)
	if err != nil {
		return "", err
	}
	if err := check(code); err != nil {
		return "", err
	}
	if err := s.save(n, code); err != nil {
		return "", err
	}
	url := s.url(n)
	s.mu.Lock()
	first := !s.opened[n]
	s.opened[n] = true
	s.last = url
	if _, known := s.seen[n]; !known {
		// From here on every answer on this page is news.
		s.seen[n] = len(readAnswers(answersPath(s.codePath(n))).Log)
	}
	s.mu.Unlock()
	if s.web != nil && !s.web.Reachable() {
		return url + "\nsaved, but the web server is not serving, so this URL will not load; tell the user it was not published", nil
	}
	if first && s.open != nil {
		if err := s.open(url); err != nil {
			// Still published: say so, so the user gets the URL.
			return url + "\nthe browser did not open (" + err.Error() + "); give the user this URL", nil
		}
	}
	return url, nil
}

// Patch merges patch into the published program statement by
// statement (see merge) and returns the URL: a statement with the
// same name replaces, a new one is added, `name = null` removes.
func (s *Store) Patch(name string, patch string) (string, error) {
	n, err := cleanName(name)
	if err != nil {
		return "", err
	}
	base, err := os.ReadFile(s.codePath(n))
	if err != nil {
		return "", fmt.Errorf("artifact %q is not published in this session (publish it first with tools.artifact)", n)
	}
	if len(split(patch)) == 0 {
		return "", errors.New("artifact patch has no statements (`name = Expression` lines; `name = null` removes one)")
	}
	code := merge(string(base), patch)
	if err := check(code); err != nil {
		return "", err
	}
	if err := s.save(n, code); err != nil {
		return "", err
	}
	return s.url(n), nil
}

// save writes the program and bumps its version.
func (s *Store) save(name, code string) error {
	file := s.codePath(name)
	if err := os.MkdirAll(filepath.Dir(file), 0o755); err != nil {
		return fmt.Errorf("artifact: %w", err)
	}
	m := readMeta(metaPath(file))
	m.Version++
	m.Published = time.Now().UTC().Format(time.RFC3339)
	if !strings.HasSuffix(code, "\n") {
		code += "\n"
	}
	if err := os.WriteFile(file, []byte(code), 0o644); err != nil {
		return fmt.Errorf("artifact: %w", err)
	}
	b, _ := json.Marshal(m)
	if err := os.WriteFile(metaPath(file), b, 0o644); err != nil {
		return fmt.Errorf("artifact: %w", err)
	}
	return nil
}

func readMeta(path string) meta {
	var m meta
	if b, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(b, &m)
	}
	return m
}

func readAnswers(path string) Answers {
	a := Answers{State: map[string]any{}}
	if b, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(b, &a)
		if a.State == nil {
			a.State = map[string]any{}
		}
	}
	return a
}

// Answers is tools.artifactAnswers(name): the page's form state, the
// actions taken and the notes written, as JSON.
func (s *Store) Answers(name string) (string, error) {
	n, err := cleanName(name)
	if err != nil {
		return "", err
	}
	if _, err := os.Stat(s.codePath(n)); err != nil {
		return "", fmt.Errorf("artifact %q is not published in this session", n)
	}
	a := readAnswers(answersPath(s.codePath(n)))
	s.mu.Lock()
	s.seen[n] = len(a.Log)
	s.mu.Unlock()
	if len(a.Log) == 0 && len(a.State) == 0 {
		return "no answers yet on " + n, nil
	}
	actions := []any{}
	for _, e := range a.Log {
		if e.Kind == "action" {
			actions = append(actions, e.Value)
		}
	}
	b, _ := json.MarshalIndent(map[string]any{"state": a.State, "actions": actions, "notes": a.Notes}, "", " ")
	return string(b), nil
}

// Guide is tools.artifactGuide(): the language reference the model
// reads before its first page — kept out of the always-on prompt
// because it is long.
func (s *Store) Guide() (string, error) { return langGuide, nil }

var answerMu sync.Mutex

// record appends one thing the user did. Serialised across handlers:
// two tabs may post at once.
func record(path string, e LogEntry, state map[string]any) (Answers, error) {
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
		a.Log = append(a.Log, e)
	case "action":
		a.Log = append(a.Log, e)
	case "state":
		// Field values as typed; not news on their own, the agent reads
		// them with the next action or via tools.artifactAnswers.
	default:
		return a, fmt.Errorf("unknown kind %q", e.Kind)
	}
	if state != nil {
		a.State = state
	}
	b, err := json.MarshalIndent(a, "", " ")
	if err != nil {
		return a, err
	}
	return a, os.WriteFile(path, b, 0o644)
}

// watch tells the agent what the page reports: answers as they
// arrive, and the renderer's errors for the current version. Every
// two seconds the session's files are read; anything not yet noticed
// becomes a notice. File-based on purpose — the process serving the
// page may not be the one running this session.
func (s *Store) watch(stop <-chan struct{}) {
	t := time.NewTicker(2 * time.Second)
	defer t.Stop()
	for {
		select {
		case <-stop:
			return
		case <-t.C:
		}
		s.sweep()
	}
}

// sweep is one pass of watch.
func (s *Store) sweep() {
	files, _ := filepath.Glob(filepath.Join(s.root, s.session, "*.ui"))
	for _, f := range files {
		name := strings.TrimSuffix(filepath.Base(f), ".ui")
		a := readAnswers(answersPath(f))
		s.mu.Lock()
		seen, known := s.seen[name]
		if !known {
			// First sight: a resumed session does not replay old answers.
			seen = len(a.Log)
		}
		s.seen[name] = len(a.Log)
		mine := s.opened[name]
		s.mu.Unlock()
		for _, e := range a.Log[min(seen, len(a.Log)):] {
			if s.notify != nil {
				s.notify(noticeText(name, e))
			}
		}
		// The renderer's errors: reported once per distinct report, and
		// only for pages this process published (a resumed session's
		// old complaints are not news).
		b, err := os.ReadFile(errorsPath(f))
		if err != nil {
			continue
		}
		var es Errors
		_ = json.Unmarshal(b, &es)
		key := string(b)
		s.mu.Lock()
		prev, known := s.errSeen[name]
		s.errSeen[name] = key
		s.mu.Unlock()
		if !mine || len(es.Errors) == 0 || (known && prev == key) {
			continue
		}
		if s.notify != nil {
			s.notify(errorText(name, es))
		}
	}
}

// noticeText is one answer as the agent reads it.
func noticeText(name string, e LogEntry) string {
	switch e.Kind {
	case "note":
		return fmt.Sprintf("[artifact %s] the user wrote: %s", name, e.Value)
	default:
		m, _ := e.Value.(map[string]any)
		msg, _ := m["message"].(string)
		typ, _ := m["type"].(string)
		out := fmt.Sprintf("[artifact %s] the user pressed %q", name, msg)
		if form, _ := m["form"].(string); form != "" {
			out += fmt.Sprintf(" on form %q", form)
		}
		if typ != "" && typ != "continue_conversation" {
			out += " (" + typ + ")"
		}
		if fs, ok := m["formState"].(map[string]any); ok && len(fs) > 0 {
			b, _ := json.Marshal(fs)
			out += " with " + string(b)
		}
		return out + fmt.Sprintf(" — tools.artifactAnswers(%q) has everything", name)
	}
}

// errorText is the renderer's complaint as the agent reads it: one
// line per error, with the statement and the hint when given.
func errorText(name string, es Errors) string {
	var b strings.Builder
	fmt.Fprintf(&b, "[artifact %s] the page has %d error(s) (version %d) — fix them with tools.artifactPatch:", name, len(es.Errors), es.Version)
	for _, e := range es.Errors {
		b.WriteString("\n- ")
		if id, _ := e["statementId"].(string); id != "" {
			fmt.Fprintf(&b, "%s: ", id)
		}
		msg, _ := e["message"].(string)
		b.WriteString(msg)
		if hint, _ := e["hint"].(string); hint != "" {
			b.WriteString(" — " + hint)
		}
	}
	return b.String()
}

// entry is one published page as the index lists it.
type entry struct {
	Session, Name, Published string
	Version                  int
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
			if !strings.HasSuffix(f.Name(), ".ui") {
				continue
			}
			file := filepath.Join(s.root, sess, f.Name())
			m := readMeta(metaPath(file))
			out = append(out, entry{Session: sess, Name: strings.TrimSuffix(f.Name(), ".ui"), Published: m.Published, Version: m.Version})
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
		fmt.Fprintf(&b, "%s (v%d)  %s/artifacts/%s/%s\n", e.Name, e.Version, s.web.URL(), e.Session, e.Name)
	}
	fmt.Fprintf(&b, "all sessions: %s/artifacts/", s.web.URL())
	return b.String()
}

// ServeHTTP serves /artifacts/ (the index), /artifacts/_lib/*, and
// /artifacts/<session>/<name>[.ui | /answers | /errors | /events].
func (s *Store) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	rest := strings.TrimPrefix(path.Clean(r.URL.Path), "/artifacts")
	rest = strings.TrimPrefix(rest, "/")
	switch {
	case rest == "":
		s.serveIndex(w)
	case rest == "_lib/openui-bundle.min.js":
		w.Header().Set("Content-Type", "application/javascript")
		w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		_, _ = w.Write(bundleJS)
	case rest == "_lib/openui-styles.css":
		w.Header().Set("Content-Type", "text/css")
		w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		_, _ = w.Write(bundleCSS)
	default:
		sess, name, ok := strings.Cut(rest, "/")
		if !ok || strings.Contains(sess, "..") || strings.Contains(name, "..") {
			http.NotFound(w, r)
			return
		}
		name, sub, _ := strings.Cut(name, "/")
		raw := strings.HasSuffix(name, ".ui")
		name = strings.TrimSuffix(name, ".ui")
		file := filepath.Join(s.root, sess, name+".ui")
		code, err := os.ReadFile(file)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		switch sub {
		case "answers":
			s.serveAnswers(w, r, answersPath(file))
			return
		case "errors":
			serveErrors(w, r, errorsPath(file))
			return
		case "events":
			serveEvents(w, r, file)
			return
		case "":
		default:
			http.NotFound(w, r)
			return
		}
		if raw {
			w.Header().Set("Content-Type", "text/plain; charset=utf-8")
			_, _ = w.Write(code)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write([]byte(render(name, string(code), readMeta(metaPath(file)))))
	}
}

// serveAnswers reads (GET) or records (POST {kind, value, state?}) a
// page's answers.
func (s *Store) serveAnswers(w http.ResponseWriter, r *http.Request, path string) {
	w.Header().Set("Content-Type", "application/json")
	switch r.Method {
	case http.MethodGet:
		_ = json.NewEncoder(w).Encode(readAnswers(path))
	case http.MethodPost:
		var body struct {
			Kind  string         `json:"kind"`
			Value any            `json:"value"`
			State map[string]any `json:"state"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&body); err != nil {
			http.Error(w, `{"error":"bad json"}`, 400)
			return
		}
		a, err := record(path, LogEntry{Kind: body.Kind, Value: body.Value}, body.State)
		if err != nil {
			http.Error(w, `{"error":`+strconv.Quote(err.Error())+`}`, 400)
			return
		}
		_ = json.NewEncoder(w).Encode(a)
	default:
		http.Error(w, "method", 405)
	}
}

// serveErrors records (POST) what the renderer reported for a version,
// or reads it back (GET). An empty list means the page rendered clean.
func serveErrors(w http.ResponseWriter, r *http.Request, path string) {
	w.Header().Set("Content-Type", "application/json")
	switch r.Method {
	case http.MethodGet:
		b, err := os.ReadFile(path)
		if err != nil {
			_, _ = w.Write([]byte(`{"version":0,"errors":[]}`))
			return
		}
		_, _ = w.Write(b)
	case http.MethodPost:
		var es Errors
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&es); err != nil {
			http.Error(w, `{"error":"bad json"}`, 400)
			return
		}
		if es.Errors == nil {
			es.Errors = []map[string]any{}
		}
		b, _ := json.MarshalIndent(es, "", " ")
		if err := os.WriteFile(path, b, 0o644); err != nil {
			http.Error(w, `{"error":"write"}`, 500)
			return
		}
		_, _ = w.Write(b)
	default:
		http.Error(w, "method", 405)
	}
}

// serveEvents is the page's live-reload stream: server-sent events,
// one "update" whenever the program changes on disk (a publish or a
// patch from any process), a comment every 15 s to keep it open.
func serveEvents(w http.ResponseWriter, r *http.Request, file string) {
	fl, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "no streaming", 500)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	last := mtime(file)
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
		if m := mtime(file); m != last {
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

// render is the viewer with the program inlined, so a page is one
// request (plus the bundle, cached forever).
func render(name, code string, m meta) string {
	page := map[string]any{"name": name, "code": code, "version": m.Version, "published": m.Published}
	b, _ := json.Marshal(page)
	safe := strings.ReplaceAll(string(b), "</", `<\/`)
	return strings.Replace(viewerHTML, "/*PAGE*/null", safe, 1)
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
		fmt.Fprintf(&b, `<li><a href="/artifacts/%s/%s">%s</a> <small>v%d · %s · %s</small></li>`, e.Session, e.Name, html(e.Name), e.Version, html(e.Session), html(e.Published))
	}
	b.WriteString("</ul>")
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_, _ = w.Write([]byte(b.String()))
}

func html(s string) string {
	return strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;").Replace(s)
}

// PromptSection tells the model when a page earns its place and how
// to write one. The language reference itself is behind
// tools.artifactGuide(): long, and needed once.
const PromptSection = `Artifacts — pages you publish for the user to read and act on in the browser: tools.artifact(name, code) -> URL.
Make one when the user will scan, compare, keep or interact with the result: a comparison, a report, a dashboard, a plan, a form. Not for a short answer, and not to dress prose up — reply in text then.
code is OpenUI Lang: one "name = Component(...)" statement per line, root = Card([...]) first, positional arguments, references to other statements, $variables for reactive state. Before your FIRST page in a session call tools.artifactGuide() and read the component signatures; do not guess them.
Example:
  root = Card([head, stats, tbl, ask])
  head = CardHeader("Embedded database comparison", "measured on this machine")
  stats = TextContent("SQLite writes 8x faster; DuckDB scans 6x faster.", "large")
  tbl = Table([Col("engine", ["SQLite", "DuckDB"]), Col("p99 write ms", [1.8, 14.2], "number")])
  ask = Buttons([Button("Keep SQLite"), Button("Try DuckDB")])
The page talks back: a button, a form submit or a follow-up reaches you as a notice while you work or wait (tools.artifactAnswers(name) has the form state), and so do the renderer's errors — fix those with tools.artifactPatch(name, statements) (same-named statements replace, new ones add, "name = null" removes) rather than resending the page. When you need the user to choose, publish and end your turn; the answer wakes you. Reply with the URL and one line on what the page shows. Only real data on a page, never invented numbers.`

type plugin struct{}

func init() {
	kernel.Register("artifacts", func() kernel.Plugin { return plugin{} })
}

// openLatest opens the page published most recently (the index when
// none), whether or not Publish already opened it, and says what
// happened: a browser that would not open still leaves the URL.
func (s *Store) openLatest() string {
	s.mu.Lock()
	url := s.last
	s.mu.Unlock()
	if url == "" {
		url = s.web.URL() + "/artifacts/"
	}
	if s.open == nil {
		return url
	}
	if err := s.open(url); err != nil {
		return "the browser did not open (" + err.Error() + "): " + url
	}
	return "opened " + url
}

func (plugin) Name() string     { return "artifacts" }
func (plugin) Inject() []string { return []string{"codemode", "web"} }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	s := &Store{opened: map[string]bool{}, seen: map[string]int{}, errSeen: map[string]string{}, open: web.Open}
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
	code.RegisterTool("artifactGuide", s.Guide)
	if d, ok := code.(describer); ok {
		d.Describe("artifact", `tools.artifact(name, code) publishes a page for the user (code = OpenUI Lang, see Artifacts) and returns its URL.`)
		d.Describe("artifactPatch", `tools.artifactPatch(name, statements) changes a published page statement by statement; the open page reloads itself.`)
		d.Describe("artifactAnswers", `tools.artifactAnswers(name) -> JSON: the page's form state, the buttons pressed, the notes written.`)
		d.Describe("artifactGuide", `tools.artifactGuide() -> the OpenUI Lang reference: syntax and every component's signature. Read it before your first page.`)
	}
	ctx.Effect(func() {
		for _, n := range []string{"artifact", "artifactPatch", "artifactAnswers", "artifactGuide"} {
			code.RegisterTool(n, nil)
		}
	})
	if n, err := kernel.Get[notifier](ctx, "job-notices"); err == nil {
		s.notify = n.Notify
	}
	stop := make(chan struct{})
	go s.watch(stop)
	ctx.Effect(func() { close(stop) })

	if sec, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		sec.Set("artifacts", PromptSection)
		ctx.Effect(func() { sec.Set("artifacts", "") })
	}
	if reg, err := kernel.Get[*commands.Registry](ctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "artifacts", Usage: "[open]", Summary: "pages published this session, with their URLs; open = the latest in the browser"}
		run := func(args string) (string, error) {
			if args == "open" {
				return s.openLatest(), nil
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
