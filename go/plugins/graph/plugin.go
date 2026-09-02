package graph

// The "graph" plugin: opens the store, records this session, injects the
// workspace's neighborhood as a prompt section (passive memory), binds
// the narrow verbs as tools.graph.* in codemode (search, neighbors,
// timeline, resolve; assert, invalidate), and contributes `bough graph`.
// The model never writes rows directly: the two write verbs stamp
// author and episode themselves, so provenance cannot be skipped.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/dop251/goja"

	"github.com/andreylukin/bough/kernel"
)

const promptSectionName = "memory"

// codemode is the slice of the codemode service the plugin uses.
type codemode interface {
	WithVM(fn func(vm *goja.Runtime, tools *goja.Object) error) error
}

type sections interface {
	Set(name, text string)
}

// historyPath is the slice of the history service that names this
// session (the file base name is the session id).
type historyPath interface {
	Path() string
}

// Config is the row's config.
type Config struct {
	Path    string // graph database; default ~/.bough/graph.db
	Embed   bool   // embed titles/claims when a key is in the environment (default true)
	Hops    int    // neighborhood radius for the prompt section (default 1)
	MaxRows int    // prompt section cap (default 40)
}

func parseConfig(cfg map[string]any) (Config, error) {
	home, _ := os.UserHomeDir()
	c := Config{Path: filepath.Join(home, ".bough", "graph.db"), Embed: true, Hops: 1, MaxRows: 40}
	for k, v := range cfg {
		switch k {
		case "path":
			s, ok := v.(string)
			if !ok || s == "" {
				return c, fmt.Errorf("graph: path must be a non-empty string")
			}
			c.Path = s
		case "embed":
			b, ok := v.(bool)
			if !ok {
				return c, fmt.Errorf("graph: embed must be a bool")
			}
			c.Embed = b
		case "hops":
			n, ok := toInt(v)
			if !ok || n < 1 || n > 3 {
				return c, fmt.Errorf("graph: hops must be 1..3")
			}
			c.Hops = n
		case "max_rows":
			n, ok := toInt(v)
			if !ok || n < 1 {
				return c, fmt.Errorf("graph: max_rows must be >= 1")
			}
			c.MaxRows = n
		default:
			return c, fmt.Errorf("graph: unknown config key %q", k)
		}
	}
	return c, nil
}

func toInt(v any) (int, bool) {
	switch n := v.(type) {
	case int:
		return n, true
	case int64:
		return int(n), true
	case float64:
		return int(n), n == float64(int(n))
	case string:
		i, err := strconv.Atoi(n)
		return i, err == nil
	}
	return 0, false
}

// Service is the "graph" service other plugins (and tests) use.
type Service struct {
	Store   *Store
	session Entity // this session's entity; zero when history is absent
	episode int64  // this session's episode, what the model's asserts cite
}

type plugin struct{}

func init() {
	kernel.Register("graph", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "graph" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	c, err := parseConfig(cfg)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(c.Path), 0o755); err != nil {
		return err
	}
	st, err := Open(c.Path)
	if err != nil {
		return err
	}
	ctx.Effect(func() { st.Close() })
	if c.Embed {
		st.SetEmbedder(EmbedderFromEnv())
	}
	svc := &Service{Store: st}

	// This session: an entity, an episode for what the model asserts, and
	// the deterministic touches edges (repo, branch → ticket).
	cwd, _ := os.Getwd()
	ws := Workspace(cwd)
	sessionID := ""
	if h, err := kernel.Get[historyPath](ctx, "history"); err == nil && h.Path() != "" {
		sessionID = strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
	}
	if sessionID == "" {
		sessionID = "pid:" + strconv.Itoa(os.Getpid()) + ":" + time.Now().UTC().Format(time.RFC3339)
	}
	if ep, err := st.Episode("session", sessionID); err == nil {
		svc.episode = ep
		if se, err := st.Upsert("session", sessionID, "", ""); err == nil {
			svc.session = se
			svc.recordWorkspace(ws)
		}
	}

	ctx.Provide("graph", svc)

	// Passive injection: the workspace's neighborhood, once per mount.
	if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
		s.Set(promptSectionName, svc.PromptSection(ws, c.Hops, c.MaxRows))
		ctx.Effect(func() { s.Set(promptSectionName, "") })
	}
	// The verbs, as tools.graph.* in the model's runtime.
	if cm, err := kernel.Get[codemode](ctx, "codemode"); err == nil {
		if err := cm.WithVM(func(vm *goja.Runtime, tools *goja.Object) error {
			return tools.Set("graph", svc.jsObject(vm))
		}); err != nil {
			return err
		}
	}
	return nil
}

// WorkspaceInfo is what the cwd resolves to.
type WorkspaceInfo struct {
	Dir     string
	Repo    string // repo key, "" outside a checkout
	Branch  string
	Tickets []string // from the branch name
}

// Workspace resolves a directory: git origin and branch, tickets in the
// branch name. Never fails; fields are empty outside a checkout.
func Workspace(dir string) WorkspaceInfo {
	w := WorkspaceInfo{Dir: dir}
	if origin := gitOrigin(dir); origin != "" {
		w.Repo = RepoKey(origin)
	}
	w.Branch = gitBranch(dir)
	w.Tickets = Tickets(w.Branch)
	return w
}

func gitOrigin(dir string) string {
	out, err := exec.Command("git", "-C", dir, "config", "--get", "remote.origin.url").Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

func gitBranch(dir string) string {
	out, err := exec.Command("git", "-C", dir, "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

// recordWorkspace links this session to the repo it runs in and to the
// tickets its branch names. Deterministic, author "session".
func (s *Service) recordWorkspace(ws WorkspaceInfo) {
	if s.session.ID == 0 {
		return
	}
	now := s.Store.unix()
	if ws.Repo != "" {
		if re, err := s.Store.Upsert("repo", ws.Repo, RepoName(ws.Repo), ""); err == nil {
			_, _ = s.Store.Assert(s.session, "touches", re, s.episode, "session", AssertOpts{ValidFrom: now, ObservedAt: now})
		}
	}
	for _, t := range ws.Tickets {
		if te, err := s.Store.Upsert("ticket", t, t, ""); err == nil {
			_, _ = s.Store.Assert(s.session, "touches", te, s.episode, "session", AssertOpts{ValidFrom: now, ObservedAt: now})
		}
	}
}

// PromptSection renders the workspace's 1-2 hop neighborhood: the repo,
// the branch's tickets, and every open edge around them, capped. Empty
// when the graph knows nothing about here, so a fresh install adds no
// bytes to the prompt.
func (s *Service) PromptSection(ws WorkspaceInfo, hops, maxRows int) string {
	var seeds []Entity
	if ws.Repo != "" {
		if e, err := s.Store.Get("repo", ws.Repo); err == nil {
			seeds = append(seeds, e)
		}
	}
	for _, t := range ws.Tickets {
		if e, err := s.Store.Get("ticket", t); err == nil {
			seeds = append(seeds, e)
		}
	}
	if len(seeds) == 0 {
		return ""
	}
	seen := map[int64]bool{}
	var lines []string
	for _, seed := range seeds {
		edges, err := s.Store.Neighbors(seed, hops, "", 0)
		if err != nil {
			continue
		}
		for _, e := range edges {
			if seen[e.ID] || e.Src.ID == s.session.ID || e.Dst.ID == s.session.ID {
				continue
			}
			seen[e.ID] = true
			if e.Src.Kind == "session" && e.Rel == "ran" {
				continue // command noise is for search, not the prompt
			}
			lines = append(lines, "- "+describe(e))
		}
	}
	if len(lines) == 0 {
		return ""
	}
	sort.Strings(lines)
	if len(lines) > maxRows {
		lines = append(lines[:maxRows], fmt.Sprintf("- … %d more; tools.graph.neighbors(key) for the rest", len(lines)-maxRows))
	}
	head := "## memory\nWhat the graph knows about this workspace (" + ws.Repo
	if len(ws.Tickets) > 0 {
		head += ", " + strings.Join(ws.Tickets, " ")
	}
	head += "). Look further with tools.graph.search(q), .neighbors(key), .timeline(key), .resolve(ref); record a fact with tools.graph.assert(src, rel, dst, evidence).\n"
	return head + strings.Join(lines, "\n")
}

// describe renders one edge for a prompt line.
func describe(e Edge) string {
	if e.Claim != "" {
		return fmt.Sprintf("%s (%s, %s)", e.Claim, e.Author, time.Unix(e.ObservedAt, 0).Format("2006-01-02"))
	}
	return fmt.Sprintf("%s %s %s (%s, %s)", label(e.Src), e.Rel, label(e.Dst), e.Author, time.Unix(e.ObservedAt, 0).Format("2006-01-02"))
}

func label(e Entity) string {
	if e.Title != "" && e.Title != e.Key {
		return e.Kind + ":" + e.Key + " “" + truncate(e.Title, 60) + "”"
	}
	return e.Kind + ":" + e.Key
}

// Resolve turns a free-form reference into an entity: a key that
// exists, else the parsed kind/key when the graph has it.
func (s *Service) Resolve(ref string) (Entity, error) {
	r, ok := ParseRef(ref)
	if !ok {
		return Entity{}, fmt.Errorf("graph: cannot read %q as a ticket, pr, repo, person, branch or slug", ref)
	}
	if e, err := s.Store.Get(r.Kind, r.Key); err == nil {
		return e, nil
	}
	// A slug may be any kind; try them all before giving up.
	for _, kind := range []string{"concept", "ticket", "pr", "repo", "person", "session", "command", "url"} {
		if e, err := s.Store.Get(kind, r.Key); err == nil {
			return e, nil
		}
	}
	return Entity{}, fmt.Errorf("graph: no entity for %q (%s %s)", ref, r.Kind, r.Key)
}

// AssertRef is the model's write verb: src and dst are references or
// "kind:key" pairs; unknown entities are created with the key as title.
// The edge cites this session's episode and is signed "session".
func (s *Service) AssertRef(src, rel, dst, evidence string) (Edge, error) {
	if s.episode == 0 {
		return Edge{}, errors.New("graph: no session episode to cite")
	}
	a, err := s.entityFor(src)
	if err != nil {
		return Edge{}, err
	}
	b, err := s.entityFor(dst)
	if err != nil {
		return Edge{}, err
	}
	return s.Store.Assert(a, rel, b, s.episode, "session", AssertOpts{Claim: evidence})
}

// entityFor accepts "kind:key" or a parseable reference, creating the
// entity when absent.
func (s *Service) entityFor(ref string) (Entity, error) {
	if e, err := s.Resolve(ref); err == nil {
		return e, nil
	}
	kind, key, ok := strings.Cut(ref, ":")
	if !ok || kind == "" || key == "" || strings.ContainsAny(kind, " /") {
		r, ok := ParseRef(ref)
		if !ok {
			return Entity{}, fmt.Errorf("graph: %q is neither kind:key nor a known reference", ref)
		}
		kind, key = r.Kind, r.Key
	}
	return s.Store.Upsert(kind, key, key, "")
}

// jsObject binds the verbs. Results are plain values goja renders as
// JSON-like objects; errors become exceptions in the calling script.
func (s *Service) jsObject(vm *goja.Runtime) *goja.Object {
	o := vm.NewObject()
	throw := func(err error) { panic(vm.NewGoError(err)) }
	o.Set("search", func(query string, limit int) any {
		hits, err := s.Store.Search(context.Background(), query, limit)
		if err != nil {
			throw(err)
		}
		return plain(hits)
	})
	o.Set("neighbors", func(ref string, hops int, rel string) any {
		e, err := s.Resolve(ref)
		if err != nil {
			throw(err)
		}
		edges, err := s.Store.Neighbors(e, hops, rel, 0)
		if err != nil {
			throw(err)
		}
		return plain(edges)
	})
	o.Set("timeline", func(ref string) any {
		e, err := s.Resolve(ref)
		if err != nil {
			throw(err)
		}
		edges, err := s.Store.Timeline(e)
		if err != nil {
			throw(err)
		}
		return plain(edges)
	})
	o.Set("resolve", func(ref string) any {
		e, err := s.Resolve(ref)
		if err != nil {
			throw(err)
		}
		return plain(e)
	})
	o.Set("assert", func(src, rel, dst, evidence string) any {
		e, err := s.AssertRef(src, rel, dst, evidence)
		if err != nil {
			throw(err)
		}
		return plain(e)
	})
	o.Set("invalidate", func(edge int64, reason string) any {
		if err := s.Store.Invalidate(edge, reason, "session", 0); err != nil {
			throw(err)
		}
		return true
	})
	return o
}

// plain round-trips through JSON so goja sees maps and slices with the
// json field names, not Go structs.
func plain(v any) any {
	b, _ := json.Marshal(v)
	var out any
	_ = json.Unmarshal(b, &out)
	return out
}

// Commands implements kernel.Commander: `bough graph …`.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "graph",
		Usage:   "stats | backfill | search <q> | neighbors <ref> [hops] | timeline <ref> | resolve <ref>",
		Summary: "the memory graph: counts, backfill from bough.db + history, and the read verbs",
		Run:     runCLI,
	}}
}

func runCLI(cfg map[string]any, args []string) error {
	const usage = "usage: bough graph stats | backfill | search <q> | neighbors <ref> [hops] | timeline <ref> | resolve <ref>"
	if len(args) == 0 {
		return errors.New(usage)
	}
	c, err := parseConfig(cfg)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(c.Path), 0o755); err != nil {
		return err
	}
	st, err := Open(c.Path)
	if err != nil {
		return err
	}
	defer st.Close()
	if c.Embed {
		st.SetEmbedder(EmbedderFromEnv())
	}
	svc := &Service{Store: st}
	home, _ := os.UserHomeDir()
	out := func(v any) error {
		b, err := json.MarshalIndent(v, "", "  ")
		if err != nil {
			return err
		}
		fmt.Println(string(b))
		return nil
	}
	switch args[0] {
	case "stats":
		s, err := st.Stats()
		if err != nil {
			return err
		}
		fmt.Printf("graph    %s\nentities %d\nedges    %d (%d open)\nepisodes %d\nembedded %d\n", c.Path, s.Entities, s.Edges, s.OpenEdges, s.Episodes, s.Embeddings)
		if st.embed == nil {
			fmt.Println("embed    off (no OPENROUTER_API_KEY / OPENAI_API_KEY): FTS-only search")
		} else {
			fmt.Printf("embed    %s\n", st.embed.Model())
		}
		return nil
	case "backfill":
		r, err := st.Backfill(filepath.Join(home, ".bough", "bough.db"), filepath.Join(home, ".bough", "history"))
		if err != nil {
			return err
		}
		fmt.Printf("concepts %d, cites %d, commands %d, repos %d, sessions %d\n", r.Concepts, r.Cites, r.Commands, r.Repos, r.Sessions)
		for _, sk := range r.Skipped {
			fmt.Println("skipped:", sk)
		}
		return nil
	case "search":
		if len(args) < 2 {
			return errors.New(usage)
		}
		hits, err := st.Search(context.Background(), strings.Join(args[1:], " "), 10)
		if err != nil {
			return err
		}
		return out(hits)
	case "neighbors":
		if len(args) < 2 {
			return errors.New(usage)
		}
		e, err := svc.Resolve(args[1])
		if err != nil {
			return err
		}
		hops := 1
		if len(args) > 2 {
			hops, _ = strconv.Atoi(args[2])
		}
		edges, err := st.Neighbors(e, hops, "", 0)
		if err != nil {
			return err
		}
		return out(edges)
	case "timeline":
		if len(args) < 2 {
			return errors.New(usage)
		}
		e, err := svc.Resolve(args[1])
		if err != nil {
			return err
		}
		edges, err := st.Timeline(e)
		if err != nil {
			return err
		}
		return out(edges)
	case "resolve":
		if len(args) < 2 {
			return errors.New(usage)
		}
		e, err := svc.Resolve(args[1])
		if err != nil {
			return err
		}
		return out(e)
	}
	return errors.New(usage)
}
