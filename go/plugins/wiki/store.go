package wiki

// The wiki as the control room reads it. The CLI above compiles and
// checks; this reads the result back as things a person can look at:
// pages split into claims, each claim in one of a few states that are
// all mechanically knowable — never a confidence score.
//
//   cited       carries at least one `<session>#<seq>` that resolves
//   inferred    starts with *Inference:* (the skill's label)
//   uncited     a claim with neither
//   unsupported a citation `wiki check` cannot resolve
//   superseded  the skill's **Outdated** (superseded by `…`) marker

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode"

	"github.com/andreylukin/bough/plugins/history"
)

var (
	ErrNotFound = errors.New("wiki: not found")
	ErrBadPath  = errors.New("wiki: not a page path")
	// ErrStale is a claim edit against text that has changed since the
	// page was read: an ingest ran in between, and line numbers moved.
	ErrStale = errors.New("wiki: the page changed since it was read")
)

// Store reads and edits one wiki. It holds no state: every answer is
// read off disk, so it never disagrees with the files an ingest just
// wrote.
type Store struct{ p paths }

// Open is the wiki under a home directory (~/.bough/wiki).
func Open(home string) *Store {
	b := filepath.Join(home, ".bough")
	return &Store{paths{
		hist:  filepath.Join(b, "history"),
		wiki:  filepath.Join(b, "wiki"),
		skill: filepath.Join(b, "skills", "llm-wiki", "SKILL.md"),
		home:  home,
	}}
}

func (s *Store) Dir() string { return s.p.wiki }

// Counts is how a page's claims split across the states.
type Counts struct {
	Cited       int `json:"cited"`
	Inferred    int `json:"inferred"`
	Uncited     int `json:"uncited"`
	Unsupported int `json:"unsupported"`
	Superseded  int `json:"superseded"`
}

func (c Counts) flagged() int { return c.Uncited + c.Unsupported + c.Superseded }

// Cite is one citation, with the entry it points at as the digest
// reads it. Problem is set when the entry cannot be found.
//
// A citation is usually a history entry. The brief (topics/me) also
// cites what it read outside history — a pull request, a Slack message,
// a Linear issue, a Notion page, a commit — as `<source>:<ref>`. Those
// carry Source and Ref instead of Session and Seq, and URL when the ref
// has an address; nothing resolves them, since the evidence is not on
// this machine, so they are never unsupported.
type Cite struct {
	Session string `json:"session"`
	Seq     int64  `json:"seq"`
	Source  string `json:"source,omitempty"`
	Ref     string `json:"ref,omitempty"`
	URL     string `json:"url,omitempty"`
	Label   string `json:"label"`
	Excerpt string `json:"excerpt"`
	At      string `json:"at,omitempty"`
	Problem string `json:"problem,omitempty"`
}

// Block is one piece of a page: its lede, a heading, a claim, a code
// fence, or a list of links (See also). Line and End are 1-based and
// inclusive; Raw is those lines, which a claim edit must still match.
type Block struct {
	Kind         string `json:"kind"`
	Text         string `json:"text"`
	Line         int    `json:"line"`
	End          int    `json:"end"`
	Raw          string `json:"raw"`
	Bullet       bool   `json:"bullet"`
	State        string `json:"state,omitempty"`
	Cites        []Cite `json:"cites"`
	SupersededBy *Cite  `json:"supersededBy,omitempty"`
}

// PageRef is a page as a list shows it.
type PageRef struct {
	Path    string `json:"path"`
	Topic   string `json:"topic"`
	Title   string `json:"title"`
	Summary string `json:"summary"`
	Updated string `json:"updated"`
	Counts  Counts `json:"counts"`
	// Problems says the page itself is damaged (an ingest wrote editor
	// line numbers into it, say), so it is not rendered as if it were fine.
	Problems []string `json:"problems,omitempty"`
}

var (
	lineNumRE      = regexp.MustCompile(`^\s*\d+\|`)
	emptySessionRE = regexp.MustCompile(`Sessions:\s*,{2,}`)
)

// malformed names what is wrong with a compiled page's text: lines that
// start with an editor's "N|" gutter, or a Sessions list of bare commas.
func malformed(body string) []string {
	var out []string
	numbered, nonEmpty := 0, 0
	for _, l := range strings.Split(body, "\n") {
		if strings.TrimSpace(l) == "" {
			continue
		}
		nonEmpty++
		if lineNumRE.MatchString(l) {
			numbered++
		}
	}
	if numbered >= 2 && numbered*4 >= nonEmpty {
		out = append(out, fmt.Sprintf("%d of %d lines start with editor line numbers (\"N|\"); the page was written from a numbered view and needs recompiling", numbered, nonEmpty))
	}
	if emptySessionRE.MatchString(body) {
		out = append(out, "the Sessions line is a list of empty values")
	}
	return out
}

// Page is one page, read for display.
type Page struct {
	PageRef
	Blocks     []Block   `json:"blocks"`
	Sessions   []string  `json:"sessions"`
	LinkedFrom []PageRef `json:"linkedFrom"`
	Body       string    `json:"body"`
}

var (
	indexTopicRE = regexp.MustCompile(`^## (.+?)\s*$`)
	indexLineRE  = regexp.MustCompile(`^- \[(.+?)\]\(([^)\s]+\.md)\)\s*(?:—|–|-|:)?\s*(.*)$`)
	bulletRE     = regexp.MustCompile(`^([-*]\s+)`)
	inferRE      = regexp.MustCompile(`(?i)^\*{0,2}inference:?\*{0,2}:?\s*`)
	outdatedRE   = regexp.MustCompile("(?i)^\\*{0,2}outdated\\*{0,2}\\s*(?:\\(superseded by\\s+`([A-Za-z0-9][A-Za-z0-9:._-]*)#(\\d+)`\\s*\\))?\\s*:?\\s*")
	spacesRE     = regexp.MustCompile(`[ \t]{2,}`)
	punctRE      = regexp.MustCompile(`\s+([.,;:)])`)
)

// parsePage splits a page into blocks. Citations stay unresolved: the
// caller decides whether reading history is worth it.
func parsePage(rel, body string) Page {
	pg := Page{PageRef: PageRef{Path: rel, Topic: topicOf(rel), Problems: malformed(body)}, Body: body, Blocks: []Block{}, Sessions: []string{}, LinkedFrom: []PageRef{}}
	lines := strings.Split(body, "\n")
	var cur *Block
	var buf []string
	heading := ""
	inFence := false
	flush := func() {
		if cur == nil {
			return
		}
		cur.Raw = strings.Join(lines[cur.Line-1:cur.End], "\n")
		text := strings.TrimSpace(strings.Join(buf, "\n"))
		if cur.Kind == "code" {
			cur.Text = text
			pg.Blocks = append(pg.Blocks, *cur)
			cur, buf = nil, nil
			return
		}
		if text == "" {
			cur, buf = nil, nil
			return
		}
		switch {
		case heading == "" && pg.Summary == "" && !cur.Bullet:
			cur.Kind = "lede"
			pg.Summary = strings.TrimSpace(strings.Join(strings.Fields(stripCites(text)), " "))
		case strings.EqualFold(heading, "See also"):
			cur.Kind = "links"
		default:
			cur.Kind = "claim"
		}
		classify(cur, text)
		pg.Blocks = append(pg.Blocks, *cur)
		cur, buf = nil, nil
	}
	for i, line := range lines {
		n := i + 1
		trim := strings.TrimSpace(line)
		if strings.HasPrefix(trim, "```") {
			if inFence {
				cur.End = n
				buf = append(buf, line)
				inFence = false
				flush()
				continue
			}
			flush()
			cur = &Block{Kind: "code", Line: n, End: n}
			buf = []string{line}
			inFence = true
			continue
		}
		if inFence {
			cur.End = n
			buf = append(buf, line)
			continue
		}
		switch {
		case pg.Title == "" && strings.HasPrefix(line, "# "):
			flush()
			pg.Title = strings.TrimSpace(line[2:])
		case strings.HasPrefix(line, "## "):
			flush()
			heading = strings.TrimSpace(line[3:])
			pg.Blocks = append(pg.Blocks, Block{Kind: "heading", Text: heading, Line: n, End: n, Raw: line, Cites: []Cite{}})
		case strings.HasPrefix(trim, "Updated:"):
			flush()
			u := strings.TrimSpace(strings.TrimPrefix(trim, "Updated:"))
			if i := strings.Index(u, "·"); i >= 0 {
				u = strings.TrimSpace(u[:i])
			}
			pg.Updated = u
			for _, m := range citeRE.FindAllStringSubmatch(trim, -1) {
				if !slices.Contains(pg.Sessions, m[1]) {
					pg.Sessions = append(pg.Sessions, m[1])
				}
			}
		case trim == "":
			flush()
		case bulletRE.MatchString(line):
			flush()
			cur = &Block{Line: n, End: n, Bullet: true}
			buf = []string{bulletRE.ReplaceAllString(line, "")}
		case cur != nil && cur.Bullet && (strings.HasPrefix(line, "  ") || strings.HasPrefix(line, "\t")):
			cur.End = n
			buf = append(buf, trim)
		default:
			if cur == nil || cur.Bullet {
				flush()
				cur = &Block{Line: n, End: n}
			}
			cur.End = n
			buf = append(buf, line)
		}
	}
	flush()
	if pg.Title == "" {
		pg.Title = strings.TrimSuffix(filepath.Base(rel), ".md")
	}
	return pg
}

// classify reads a block's text for its state and citations. The state
// before history is consulted: resolve turns a cited claim unsupported
// when a citation does not resolve.
func classify(b *Block, text string) {
	b.Cites = []Cite{}
	if m := outdatedRE.FindStringSubmatchIndex(text); m != nil && m[1] > 0 {
		sub := outdatedRE.FindStringSubmatch(text)
		if sub[1] != "" {
			seq, _ := strconv.ParseInt(sub[2], 10, 64)
			b.SupersededBy = &Cite{Session: sub[1], Seq: seq}
		}
		text = text[m[1]:]
		b.State = "superseded"
	}
	if loc := inferRE.FindStringIndex(text); loc != nil && loc[1] > 0 && b.State == "" {
		text = text[loc[1]:]
		b.State = "inferred"
	}
	// External first: `gh:owner/repo#7801` would otherwise read as a
	// session called gh:owner/repo citing entry 7801.
	for _, m := range extCiteRE.FindAllStringSubmatch(text, -1) {
		if !slices.ContainsFunc(b.Cites, func(c Cite) bool { return c.Source == m[1] && c.Ref == m[2] }) {
			b.Cites = append(b.Cites, Cite{Source: m[1], Ref: m[2], URL: extURL(m[1], m[2]), Label: m[1], Excerpt: m[2]})
		}
	}
	text = extCiteRE.ReplaceAllString(text, "")
	for _, m := range citeRE.FindAllStringSubmatch(text, -1) {
		seq, _ := strconv.ParseInt(m[2], 10, 64)
		// The same entry cited twice in one block is one piece of evidence.
		if !slices.ContainsFunc(b.Cites, func(c Cite) bool { return c.Session == m[1] && c.Seq == seq }) {
			b.Cites = append(b.Cites, Cite{Session: m[1], Seq: seq})
		}
	}
	b.Text = stripCites(text)
	if b.Kind != "claim" {
		b.State = ""
		return
	}
	if b.State == "" {
		if len(b.Cites) > 0 {
			b.State = "cited"
		} else {
			b.State = "uncited"
		}
	}
}

func stripCites(text string) string {
	t := citeRE.ReplaceAllString(extCiteRE.ReplaceAllString(text, ""), "")
	t = spacesRE.ReplaceAllString(t, " ")
	t = punctRE.ReplaceAllString(t, "$1")
	return strings.TrimSpace(t)
}

func topicOf(rel string) string {
	parts := strings.Split(rel, "/")
	if len(parts) >= 3 && parts[0] == "topics" {
		return parts[1]
	}
	return ""
}

// resolver reads each cited session once per request.
type resolver struct {
	hist    string
	entries map[string][]history.Entry // nil value: the session file is missing
}

func newResolver(p paths) *resolver {
	return &resolver{hist: p.hist, entries: map[string][]history.Entry{}}
}

func (r *resolver) session(id string) []history.Entry {
	es, ok := r.entries[id]
	if !ok {
		if strings.ContainsAny(id, `/\`) {
			r.entries[id] = nil
			return nil
		}
		es, _ = history.Read(filepath.Join(r.hist, id+".jsonl"))
		r.entries[id] = es
	}
	return es
}

// shortDuration drops the zero tails time.Duration prints: 5m0s → 5m, 1h0m0s → 1h.
func shortDuration(d time.Duration) string {
	s := d.String()
	if strings.HasSuffix(s, "m0s") {
		s = strings.TrimSuffix(s, "0s")
	}
	if strings.HasSuffix(s, "h0m") {
		s = strings.TrimSuffix(s, "0m")
	}
	return s
}

// fill resolves c; claim is the text citing it, so the excerpt can start
// at the part of the entry that claim is about rather than its first line.
func (r *resolver) fill(c *Cite, claim string) {
	if c.Source != "" {
		return // not on this machine: nothing to look up, nothing to be wrong
	}
	d, found, ok := r.cited(c.Session, c.Seq)
	if !ok {
		c.Problem = "cites a session that does not exist: " + c.Session
		return
	}
	if !found {
		c.Problem = fmt.Sprintf("cites entry #%d, which session %s does not have", c.Seq, c.Session)
		return
	}
	c.At = d.at.Format(time.RFC3339)
	c.Label, c.Excerpt = d.label, redact(excerpt(focus(d.text, claim), 6))
}

// citedEntry is one cited entry as describe reads it.
type citedEntry struct {
	label, text string
	at          time.Time
	found       bool
}

type citeKey struct {
	path string
	seq  int64
}

type citeVal struct {
	size int64
	mod  time.Time
	d    citedEntry
}

// citeCache outlives a request: every index load resolves every citation,
// and a cited session's file rarely changes after it is cited.
var (
	citeMu    sync.Mutex
	citeCache = map[citeKey]citeVal{}
)

// cited looks up one entry; ok is false when the session file is missing.
func (r *resolver) cited(id string, seq int64) (d citedEntry, found, ok bool) {
	if strings.ContainsAny(id, `/\`) {
		return d, false, false
	}
	path := filepath.Join(r.hist, id+".jsonl")
	st, err := os.Stat(path)
	if err != nil {
		return d, false, false
	}
	key := citeKey{path, seq}
	citeMu.Lock()
	v, hit := citeCache[key]
	citeMu.Unlock()
	if hit && v.size == st.Size() && v.mod.Equal(st.ModTime()) {
		return v.d, v.d.found, true
	}
	es := r.session(id)
	if es == nil {
		return d, false, false
	}
	for _, e := range es {
		if e.Seq != seq {
			continue
		}
		label, text, _, ok := describe(e)
		if !ok {
			label, text = e.Kind, ""
		}
		d = citedEntry{label: label, text: text, at: e.At, found: true}
		break
	}
	citeMu.Lock()
	citeCache[key] = citeVal{size: st.Size(), mod: st.ModTime(), d: d}
	citeMu.Unlock()
	return d, d.found, true
}

// resolve fills every citation on the page and counts its states.
func (r *resolver) resolve(pg *Page) {
	pg.Counts = Counts{}
	for i := range pg.Blocks {
		b := &pg.Blocks[i]
		broken := false
		for j := range b.Cites {
			r.fill(&b.Cites[j], b.Text)
			if b.Cites[j].Problem != "" {
				broken = true
			}
		}
		if b.SupersededBy != nil {
			r.fill(b.SupersededBy, b.Text)
		}
		if b.Kind != "claim" {
			continue
		}
		if broken {
			b.State = "unsupported"
		}
		switch b.State {
		case "cited":
			pg.Counts.Cited++
		case "inferred":
			pg.Counts.Inferred++
		case "uncited":
			pg.Counts.Uncited++
		case "unsupported":
			pg.Counts.Unsupported++
		case "superseded":
			pg.Counts.Superseded++
		}
		for _, c := range b.Cites {
			if c.Session != "" && !slices.Contains(pg.Sessions, c.Session) {
				pg.Sessions = append(pg.Sessions, c.Session)
			}
		}
	}
}

// focus cuts text to start at the line sharing the most words with claim
// (and, in a long line, just before the first shared word), marking the
// cut with "…". Citations carry only an entry seq, no offset, so word
// overlap is the only pointer to the span a claim rests on.
func focus(text, claim string) string {
	words := map[string]bool{}
	for _, w := range strings.FieldsFunc(strings.ToLower(stripCites(claim)), notWordRune) {
		if len([]rune(w)) >= 4 {
			words[w] = true
		}
	}
	lines := strings.Split(strings.Trim(text, "\n"), "\n")
	best, bestScore, at := 0, 0, -1
	for i, l := range lines {
		score, first := 0, -1
		lower := strings.ToLower(l)
		for w := range words {
			if k := strings.Index(lower, w); k >= 0 {
				score++
				if first < 0 || k < first {
					first = k
				}
			}
		}
		if score > bestScore {
			best, bestScore, at = i, score, first
		}
	}
	if bestScore == 0 {
		return text
	}
	line := lines[best]
	cut := best > 0
	// at indexes the lowered line; it is only valid when lowering kept byte lengths.
	if len(line) != len(strings.ToLower(line)) {
		at = 0
	}
	if r := []rune(line[:at]); len(r) > 60 {
		// Back up to a word boundary a little before the match.
		start := len(string(r[:len(r)-40]))
		if sp := strings.IndexByte(line[start:], ' '); sp >= 0 && start+sp < at {
			start += sp + 1
		}
		line, cut = line[start:], true
	}
	out := strings.Join(append([]string{line}, lines[best+1:]...), "\n")
	if cut {
		out = "… " + out
	}
	return out
}

func notWordRune(r rune) bool {
	return !unicode.IsLetter(r) && !unicode.IsDigit(r) && r != '_' && r != '-'
}

func excerpt(text string, maxLines int) string {
	lines := strings.Split(strings.Trim(text, "\n"), "\n")
	extra := 0
	if len(lines) > maxLines {
		extra = len(lines) - maxLines
		lines = lines[:maxLines]
	}
	for i, l := range lines {
		if r := []rune(l); len(r) > 300 {
			lines[i] = string(r[:300]) + "…"
		}
	}
	out := strings.Join(lines, "\n")
	if extra > 0 {
		out += fmt.Sprintf("\n… %d more lines", extra)
	}
	return out
}

// pages is every page on disk, as paths relative to the wiki.
func (s *Store) pages() []string {
	var out []string
	root := filepath.Join(s.p.wiki, "topics")
	_ = filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if d.IsDir() && strings.HasPrefix(d.Name(), ".") {
			return filepath.SkipDir
		}
		if !d.IsDir() && strings.HasSuffix(path, ".md") {
			rel, _ := filepath.Rel(s.p.wiki, path)
			out = append(out, filepath.ToSlash(rel))
		}
		return nil
	})
	slices.Sort(out)
	return out
}

// load reads and resolves every page once.
func (s *Store) load() map[string]*Page {
	r := newResolver(s.p)
	out := map[string]*Page{}
	for _, rel := range s.pages() {
		b, err := os.ReadFile(filepath.Join(s.p.wiki, filepath.FromSlash(rel)))
		if err != nil {
			continue
		}
		pg := parsePage(rel, string(b))
		r.resolve(&pg)
		if pg.Updated == "" {
			if st, err := os.Stat(filepath.Join(s.p.wiki, filepath.FromSlash(rel))); err == nil {
				pg.Updated = st.ModTime().Format("2006-01-02")
			}
		}
		out[rel] = &pg
	}
	return out
}

// links is which pages each page links to, by relative path.
func links(pages map[string]*Page) map[string][]string {
	out := map[string][]string{}
	for rel, pg := range pages {
		for _, m := range linkRE.FindAllStringSubmatch(pg.Body, -1) {
			target := filepath.ToSlash(filepath.Clean(filepath.Join(filepath.Dir(rel), m[1])))
			if target != rel && !slices.Contains(out[target], rel) {
				out[target] = append(out[target], rel)
			}
		}
	}
	return out
}

// Topic is one index heading and its pages, in index order.
type Topic struct {
	Name  string    `json:"name"`
	Pages []PageRef `json:"pages"`
}

// Health is the index's one line per component.
type Health struct {
	Installed   bool       `json:"installed"`
	Every       string     `json:"every"`
	Pending     int        `json:"pending"`
	Ingesting   bool       `json:"ingesting"`
	LastIngest  *time.Time `json:"lastIngest"`
	Unsupported int        `json:"unsupported"`
	Superseded  int        `json:"superseded"`
	Uncited     int        `json:"uncited"`
}

// Index is the wiki's front page: health, topics, and the sentence a
// graph view would otherwise be drawn for.
type Index struct {
	Dir     string  `json:"dir"`
	Exists  bool    `json:"exists"`
	Topics  []Topic `json:"topics"`
	Health  Health  `json:"health"`
	Thin    int     `json:"thin"`    // pages resting on a single citation
	Orphans int     `json:"orphans"` // pages no other page links to
}

func (s *Store) Index(now time.Time) Index {
	ix := Index{Dir: s.p.wiki, Topics: []Topic{}}
	if st, err := os.Stat(s.p.wiki); err != nil || !st.IsDir() {
		return ix
	}
	ix.Exists = true
	pages := s.load()
	incoming := links(pages)
	placed := map[string]bool{}
	topicAt := map[string]int{}
	add := func(topic string, ref PageRef) {
		i, ok := topicAt[topic]
		if !ok {
			i = len(ix.Topics)
			topicAt[topic] = i
			ix.Topics = append(ix.Topics, Topic{Name: topic, Pages: []PageRef{}})
		}
		ix.Topics[i].Pages = append(ix.Topics[i].Pages, ref)
	}
	if b, err := os.ReadFile(s.p.index()); err == nil {
		topic := ""
		for _, line := range strings.Split(string(b), "\n") {
			if m := indexTopicRE.FindStringSubmatch(line); m != nil {
				topic = m[1]
				continue
			}
			m := indexLineRE.FindStringSubmatch(line)
			if m == nil {
				continue
			}
			rel := filepath.ToSlash(filepath.Clean(m[2]))
			pg, ok := pages[rel]
			if !ok || placed[rel] {
				continue
			}
			placed[rel] = true
			ref := pg.PageRef
			if m[3] != "" {
				ref.Summary = m[3]
			}
			add(topic, ref)
		}
	}
	// A page the index does not list still exists; `wiki check` flags it,
	// and hiding it here would make that flag unfindable.
	for _, rel := range s.pages() {
		if pg, ok := pages[rel]; ok && !placed[rel] {
			add(pg.Topic, pg.PageRef)
		}
	}
	for rel, pg := range pages {
		ix.Health.Unsupported += pg.Counts.Unsupported
		ix.Health.Superseded += pg.Counts.Superseded
		ix.Health.Uncited += pg.Counts.Uncited
		cites := map[string]bool{}
		for _, b := range pg.Blocks {
			for _, c := range b.Cites {
				cites[fmt.Sprintf("%s#%d", c.Session, c.Seq)] = true
			}
		}
		if len(cites) == 1 {
			ix.Thin++
		}
		if len(incoming[rel]) == 0 {
			ix.Orphans++
		}
	}
	ix.Health.Installed, ix.Health.Every = s.schedule()
	if pend, err := FindPending(s.p, defaultQuiet, false, now); err == nil {
		ix.Health.Pending = len(pend)
	}
	for _, run := range s.runs(now) {
		if run.Running {
			ix.Health.Ingesting = true
		}
		if run.Done != nil && (ix.Health.LastIngest == nil || run.Done.After(*ix.Health.LastIngest)) {
			t := *run.Done
			ix.Health.LastIngest = &t
		}
	}
	return ix
}

var intervalRE = regexp.MustCompile(`<key>StartInterval</key>\s*<integer>(\d+)</integer>`)

// schedule reads the launchd agent `wiki install` wrote.
func (s *Store) schedule() (bool, string) {
	b, err := os.ReadFile(plistPath(s.p))
	if err != nil {
		return false, ""
	}
	if m := intervalRE.FindSubmatch(b); m != nil {
		if n, err := strconv.Atoi(string(m[1])); err == nil {
			return true, shortDuration(time.Duration(n) * time.Second)
		}
	}
	return true, ""
}

// Page reads one page with its citations resolved and its backlinks.
func (s *Store) Page(rel string) (Page, error) {
	// The Me page's "Write your profile" opens the profile before it
	// exists; a not-found page has no editor, so the person could never
	// write it from the web. It opens as the sections the brief reads,
	// and is written only when saved.
	if rel == ProfilePath {
		if _, err := os.Stat(s.p.profile()); errors.Is(err, fs.ErrNotExist) {
			return parsePage(rel, profileStarter), nil
		}
	}
	path, err := s.pagePath(rel)
	if err != nil {
		return Page{}, err
	}
	if _, err := os.Stat(path); err != nil {
		return Page{}, ErrNotFound
	}
	pages := s.load()
	pg, ok := pages[rel]
	if !ok {
		return Page{}, ErrNotFound
	}
	for _, from := range links(pages)[rel] {
		pg.LinkedFrom = append(pg.LinkedFrom, pages[from].PageRef)
	}
	return *pg, nil
}

// pagePath resolves a page path and refuses anything outside topics/.
// serve has no auth, so this is what keeps the editor a wiki editor.
func (s *Store) pagePath(rel string) (string, error) {
	if rel == "" || filepath.IsAbs(rel) || !strings.HasSuffix(rel, ".md") || strings.Contains(rel, `\`) {
		return "", ErrBadPath
	}
	clean := filepath.ToSlash(filepath.Clean(rel))
	if clean != rel || !strings.HasPrefix(clean, "topics/") || slices.Contains(strings.Split(clean, "/"), "..") {
		return "", ErrBadPath
	}
	root := filepath.Join(s.p.wiki, "topics")
	path := filepath.Join(s.p.wiki, filepath.FromSlash(clean))
	realRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		return "", ErrNotFound
	}
	real := path
	if r, err := filepath.EvalSymlinks(filepath.Dir(path)); err == nil {
		real = filepath.Join(r, filepath.Base(path))
	}
	if r, err := filepath.EvalSymlinks(path); err == nil {
		real = r
	}
	back, err := filepath.Rel(realRoot, real)
	if err != nil || back == ".." || strings.HasPrefix(back, ".."+string(filepath.Separator)) {
		return "", ErrBadPath
	}
	return path, nil
}

// WritePage replaces a page's text and commits it, so the edit is one
// reviewable change like an ingest.
func (s *Store) WritePage(rel, body string) error {
	if rel == ProfilePath {
		// The one page saved before it exists (see Page), and
		// topics/me may not exist yet either.
		if err := os.MkdirAll(s.p.me(), 0o755); err != nil {
			return err
		}
	}
	path, err := s.pagePath(rel)
	if err != nil {
		return err
	}
	if _, err := os.Stat(path); err != nil && rel != ProfilePath {
		return ErrNotFound
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		return err
	}
	commit(s.p, "edit "+rel)
	return nil
}

// Commit is one change to a page, from the wiki's git log.
type Commit struct {
	Hash    string `json:"hash"`
	At      string `json:"at"`
	Subject string `json:"subject"`
}

func (s *Store) History(rel string) ([]Commit, error) {
	if _, err := s.pagePath(rel); err != nil {
		return nil, err
	}
	out := []Commit{}
	raw, err := gitOut(s.p.wiki, "log", "--format=%h%x09%cI%x09%s", "--", rel)
	if err != nil {
		return out, nil // no git: a page with no recorded history
	}
	for _, line := range strings.Split(strings.TrimSpace(raw), "\n") {
		f := strings.SplitN(line, "\t", 3)
		if len(f) == 3 {
			out = append(out, Commit{Hash: f[0], At: f[1], Subject: f[2]})
		}
	}
	return out, nil
}

func gitOut(dir string, args ...string) (string, error) {
	if _, err := os.Stat(filepath.Join(dir, ".git")); err != nil {
		return "", err
	}
	cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
	var out bytes.Buffer
	cmd.Stdout = &out
	err := cmd.Run()
	return out.String(), err
}

// Flag is one claim review should look at, or a page-level problem
// (a broken link, a page the index does not list) when Claim is empty.
type Flag struct {
	Kind     string `json:"kind"` // unsupported | superseded | uncited | problem
	Page     string `json:"page"`
	Title    string `json:"title"`
	Line     int    `json:"line"`
	End      int    `json:"end"`
	Raw      string `json:"raw"`
	Claim    string `json:"claim"`
	Why      string `json:"why"`
	Evidence string `json:"evidence"`
	Cite     *Cite  `json:"cite,omitempty"`
}

// PendingRef is history the wiki has not compiled yet.
type PendingRef struct {
	ID      string    `json:"id"`
	Title   string    `json:"title"`
	Entries int64     `json:"entries"`
	Last    time.Time `json:"last"`
}

type Review struct {
	Flags   []Flag       `json:"flags"`
	Pending []PendingRef `json:"pending"`
}

func (s *Store) Review(now time.Time) Review {
	rv := Review{Flags: []Flag{}, Pending: []PendingRef{}}
	pages := s.load()
	covered := map[string]bool{} // page:line already explained by a claim flag
	for _, rel := range s.pages() {
		pg := pages[rel]
		if pg == nil {
			continue
		}
		for _, b := range pg.Blocks {
			if b.Kind != "claim" {
				continue
			}
			f := Flag{Kind: b.State, Page: rel, Title: pg.Title, Line: b.Line, End: b.End, Raw: b.Raw, Claim: b.Text}
			switch b.State {
			case "unsupported":
				for i := range b.Cites {
					if b.Cites[i].Problem != "" {
						c := b.Cites[i]
						f.Cite = &c
						f.Why = c.Problem
						f.Evidence = "wiki check: " + c.Problem
						break
					}
				}
			case "superseded":
				f.Why = "a later session replaced it"
				if n := b.SupersededBy; n != nil {
					c := *n
					f.Cite = &c
					f.Evidence = c.Excerpt
					if c.Problem != "" {
						f.Evidence = "wiki check: " + c.Problem
					}
				}
			case "uncited":
				f.Why = "a claim with no entry behind it"
			default:
				continue
			}
			for l := b.Line; l <= b.End; l++ {
				covered[fmt.Sprintf("%s:%d", rel, l)] = true
			}
			rv.Flags = append(rv.Flags, f)
		}
	}
	if probs, err := Check(s.p); err == nil {
		for _, pr := range probs {
			if covered[fmt.Sprintf("%s:%d", pr.Page, pr.Line)] || pr.Page == "log.md" {
				continue
			}
			title := pr.Page
			if pg := pages[pr.Page]; pg != nil {
				title = pg.Title
			}
			rv.Flags = append(rv.Flags, Flag{Kind: "problem", Page: pr.Page, Title: title, Line: pr.Line, Why: pr.Msg})
		}
	}
	if pend, err := FindPending(s.p, defaultQuiet, false, now); err == nil {
		for _, x := range pend {
			rv.Pending = append(rv.Pending, PendingRef{ID: x.ID, Title: x.Title, Entries: x.To - x.From, Last: x.Last})
		}
	}
	return rv
}

// Problems is `wiki check`, for the control room's button.
func (s *Store) Problems() ([]Problem, error) {
	if _, err := os.Stat(s.p.wiki); err != nil {
		return []Problem{}, nil
	}
	probs, err := Check(s.p)
	if probs == nil {
		probs = []Problem{}
	}
	return probs, err
}

// EditClaim applies one review decision to the lines a claim occupies:
// "inference" labels it, "drop" removes it. raw is the text the reader
// saw; if the file no longer has it there, nothing is written.
func (s *Store) EditClaim(rel string, line, end int, raw, action string) error {
	path, err := s.pagePath(rel)
	if err != nil {
		return err
	}
	b, err := os.ReadFile(path)
	if err != nil {
		return ErrNotFound
	}
	lines := strings.Split(string(b), "\n")
	if line < 1 || end < line || end > len(lines) || strings.Join(lines[line-1:end], "\n") != raw {
		return ErrStale
	}
	var msg string
	switch action {
	case "inference":
		first := lines[line-1]
		if m := bulletRE.FindStringIndex(first); m != nil {
			lines[line-1] = first[:m[1]] + "*Inference:* " + first[m[1]:]
		} else {
			lines[line-1] = "*Inference:* " + first
		}
		msg = fmt.Sprintf("review: mark %s:%d as inference", rel, line)
	case "drop":
		next := slices.Delete(slices.Clone(lines), line-1, end)
		// Removing a paragraph leaves two blank lines where one was.
		if i := line - 1; i > 0 && i < len(next) && strings.TrimSpace(next[i]) == "" && strings.TrimSpace(next[i-1]) == "" {
			next = slices.Delete(next, i, i+1)
		}
		lines = next
		msg = fmt.Sprintf("review: drop the claim at %s:%d", rel, line)
	default:
		return fmt.Errorf("wiki: unknown claim action %q", action)
	}
	if err := os.WriteFile(path, []byte(strings.Join(lines, "\n")), 0o644); err != nil {
		return err
	}
	commit(s.p, msg)
	return nil
}

// SessionRef names a session the way a header shows it.
type SessionRef struct {
	ID     string `json:"id"`
	Title  string `json:"title"`
	Repo   string `json:"repo"`
	Branch string `json:"branch"`
	Cwd    string `json:"cwd"`
}

// EntryLine is one history entry near a cited one.
type EntryLine struct {
	Seq   int64  `json:"seq"`
	Label string `json:"label"`
	Text  string `json:"text"`
}

// Source is a cited entry in place, with what surrounds it and the
// pages that cite it.
type Source struct {
	Session SessionRef  `json:"session"`
	Seq     int64       `json:"seq"`
	At      time.Time   `json:"at"`
	Total   int64       `json:"total"`
	Lines   []EntryLine `json:"lines"`
	CitedBy []PageRef   `json:"citedBy"`
}

const around = 4 // entries shown either side of a cited one

func (s *Store) Source(id string, seq int64) (Source, error) {
	if id == "" || strings.ContainsAny(id, `/\`) {
		return Source{}, ErrNotFound
	}
	es, err := history.Read(filepath.Join(s.p.hist, id+".jsonl"))
	if err != nil {
		return Source{}, ErrNotFound
	}
	src := Source{Session: SessionRef{ID: id}, Seq: seq, Lines: []EntryLine{}, CitedBy: []PageRef{}}
	var shown []history.Entry
	at := -1
	for _, e := range es {
		src.Total = max(src.Total, e.Seq)
		if e.Seq == seq {
			at = len(shown)
			src.At = e.At
			shown = append(shown, e)
			continue
		}
		if _, _, _, ok := describe(e); ok {
			shown = append(shown, e)
		}
	}
	if at < 0 {
		return Source{}, ErrNotFound
	}
	for _, e := range shown[max(0, at-around):min(len(shown), at+around+1)] {
		label, text, _, ok := describe(e)
		if !ok {
			label, text = e.Kind, ""
		}
		src.Lines = append(src.Lines, EntryLine{Seq: e.Seq, Label: label, Text: redact(excerpt(text, 20))})
	}
	if infos, err := history.List(s.p.hist); err == nil {
		for _, in := range infos {
			if in.ID == id {
				src.Session = SessionRef{ID: id, Title: in.Title, Repo: in.Repo, Branch: in.Branch, Cwd: in.Cwd}
				break
			}
		}
	}
	needle := fmt.Sprintf("`%s#%d`", id, seq)
	for _, rel := range s.pages() {
		b, err := os.ReadFile(filepath.Join(s.p.wiki, filepath.FromSlash(rel)))
		if err == nil && strings.Contains(string(b), needle) {
			src.CitedBy = append(src.CitedBy, parsePage(rel, string(b)).PageRef)
		}
	}
	return src, nil
}

// Outcome is what an ingest decided for one session, from log.md.
type Outcome struct {
	ID          string   `json:"id"`
	Title       string   `json:"title"`
	Disposition string   `json:"disposition"`
	Pages       []string `json:"pages"`
}

// IngestRun is one headless ingest session: the sessions started in the
// wiki directory are exactly these (FindPending skips them for that
// reason).
type IngestRun struct {
	Session  string     `json:"session"`
	Command  string     `json:"command"`
	At       time.Time  `json:"at"`
	Done     *time.Time `json:"done"`
	Ms       int64      `json:"ms"`
	Cost     float64    `json:"cost"`
	Running  bool       `json:"running"`
	Outcomes []Outcome  `json:"outcomes"`
	Files    []string   `json:"files"`
	Commit   string     `json:"commit"`
}

type Activity struct {
	Runs    []IngestRun `json:"runs"`
	Every   string      `json:"every"`
	Pending int         `json:"pending"`
	Today   struct {
		Runs       int     `json:"runs"`
		Ingested   int     `json:"ingested"`
		NoMaterial int     `json:"noMaterial"`
		Spent      float64 `json:"spent"`
	} `json:"today"`
	Spent float64 `json:"spent"`
}

var logLineRE = regexp.MustCompile(`(?m)^## \[[^\]]*\] ingest \| ([^#\s|]+)#\d+ \| ([^|]+?) \| (.*)$`)

const activityLimit = 50

// runs is every ingest session, oldest first, joined with what log.md
// and git recorded for it.
func (s *Store) runs(now time.Time) []IngestRun {
	infos, err := history.List(s.p.hist)
	if err != nil {
		return nil
	}
	titles := map[string]string{}
	var runs []IngestRun
	for _, in := range infos {
		titles[in.ID] = in.Title
		if in.Cwd == "" || !sameDir(in.Cwd, s.p.wiki) {
			continue
		}
		es, err := history.Read(in.Path)
		if err != nil || len(es) == 0 {
			continue
		}
		run := IngestRun{Session: in.ID, At: es[0].At, Outcomes: []Outcome{}, Files: []string{}}
		open := false
		for _, e := range es {
			switch e.Kind {
			case "input":
				if run.Command == "" {
					// The loop records the slash command with the skill it
					// expanded to appended; the command is the first line.
					text, _ := e.Data["text"].(string)
					run.Command, _, _ = strings.Cut(strings.TrimSpace(text), "\n")
				}
				open = true
			case "done":
				open = false
				t := e.At
				run.Done = &t
				run.Ms = e.At.Sub(run.At).Milliseconds()
				if u, ok := e.Data["usage"].(map[string]any); ok {
					if c, ok := u["cost"].(float64); ok {
						run.Cost += c
					}
				}
				for _, f := range strs(e.Data["files"]) {
					if !slices.Contains(run.Files, f) {
						run.Files = append(run.Files, f)
					}
				}
			}
		}
		run.Running = open && now.Sub(in.ModTime) < runTimeout
		runs = append(runs, run)
	}
	slices.SortFunc(runs, func(a, b IngestRun) int { return a.At.Compare(b.At) })

	// log.md and git both record ingests in order, so the n-th run that
	// named a session gets the n-th heading for it.
	headings := map[string][]Outcome{}
	if b, err := os.ReadFile(s.p.log()); err == nil {
		for _, m := range logLineRE.FindAllStringSubmatch(string(b), -1) {
			o := Outcome{ID: m[1], Disposition: strings.TrimSpace(m[2]), Pages: []string{}}
			for _, pg := range strings.Split(m[3], ",") {
				if pg = strings.TrimSpace(pg); pg != "" && pg != "-" {
					o.Pages = append(o.Pages, pg)
				}
			}
			headings[o.ID] = append(headings[o.ID], o)
		}
	}
	commits := map[string][]string{}
	if raw, err := gitOut(s.p.wiki, "log", "--reverse", "--format=%h%x09%s"); err == nil {
		for _, line := range strings.Split(strings.TrimSpace(raw), "\n") {
			if f := strings.SplitN(line, "\t", 2); len(f) == 2 {
				commits[f[1]] = append(commits[f[1]], f[0])
			}
		}
	}
	for i := range runs {
		ids := ingestIDs(runs[i].Command)
		for _, id := range ids {
			if hs := headings[id]; len(hs) > 0 && !runs[i].Running {
				o := hs[0]
				headings[id] = hs[1:]
				o.Title = titles[id]
				runs[i].Outcomes = append(runs[i].Outcomes, o)
			}
		}
		if len(ids) > 0 {
			subj := "ingest " + strings.Join(ids, " ")
			if cs := commits[subj]; len(cs) > 0 {
				runs[i].Commit = cs[0]
				commits[subj] = cs[1:]
			}
		}
	}
	return runs
}

func ingestIDs(command string) []string {
	f := strings.Fields(command)
	if len(f) >= 2 && f[0] == "/llm-wiki" && f[1] == "ingest" {
		return f[2:]
	}
	return nil
}

func strs(v any) []string {
	raw, _ := v.([]any)
	out := make([]string, 0, len(raw))
	for _, e := range raw {
		if s, ok := e.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func (s *Store) Activity(now time.Time) Activity {
	var a Activity
	a.Runs = []IngestRun{}
	_, a.Every = s.schedule()
	if pend, err := FindPending(s.p, defaultQuiet, false, now); err == nil {
		a.Pending = len(pend)
	}
	runs := s.runs(now)
	y, m, d := now.Date()
	for _, r := range runs {
		a.Spent += r.Cost
		if ry, rm, rd := r.At.In(now.Location()).Date(); ry == y && rm == m && rd == d {
			a.Today.Runs++
			a.Today.Spent += r.Cost
			for _, o := range r.Outcomes {
				if strings.EqualFold(o.Disposition, "No material") {
					a.Today.NoMaterial++
				} else {
					a.Today.Ingested++
				}
			}
		}
	}
	for i := len(runs) - 1; i >= 0 && len(a.Runs) < activityLimit; i-- {
		a.Runs = append(a.Runs, runs[i])
	}
	return a
}

// Hit is one page that matched a search, with the claim that matched.
type Hit struct {
	PageRef
	Excerpt string `json:"excerpt"`
}

// Search matches every term against page titles and claim text.
func (s *Store) Search(q string, limit int) []Hit {
	terms := strings.Fields(strings.ToLower(q))
	out := []Hit{}
	if len(terms) == 0 {
		return out
	}
	type scored struct {
		h     Hit
		title bool
	}
	var found []scored
	for _, rel := range s.pages() {
		b, err := os.ReadFile(filepath.Join(s.p.wiki, filepath.FromSlash(rel)))
		if err != nil {
			continue
		}
		pg := parsePage(rel, string(b))
		for _, bl := range pg.Blocks {
			if bl.Kind == "claim" && bl.State == "cited" {
				pg.Counts.Cited++
			}
		}
		all := strings.ToLower(pg.Title + "\n" + stripCites(string(b)))
		if !containsAll(all, terms) {
			continue
		}
		h := Hit{PageRef: pg.PageRef}
		for _, bl := range pg.Blocks {
			if (bl.Kind == "claim" || bl.Kind == "lede") && containsAny(strings.ToLower(bl.Text), terms) {
				h.Excerpt = excerpt(bl.Text, 3)
				break
			}
		}
		found = append(found, scored{h, containsAny(strings.ToLower(pg.Title), terms)})
	}
	slices.SortStableFunc(found, func(a, b scored) int {
		switch {
		case a.title == b.title:
			return 0
		case a.title:
			return -1
		}
		return 1
	})
	for _, f := range found {
		if len(out) == limit {
			break
		}
		out = append(out, f.h)
	}
	return out
}

func containsAll(s string, terms []string) bool {
	for _, t := range terms {
		if !strings.Contains(s, t) {
			return false
		}
	}
	return true
}

func containsAny(s string, terms []string) bool {
	for _, t := range terms {
		if strings.Contains(s, t) {
			return true
		}
	}
	return false
}
