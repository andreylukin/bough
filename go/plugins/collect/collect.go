// Package collect is the "collect" plugin: the graph's ingest from the
// external world. `bough collect [source…]` pulls what concerns me
// from GitHub (gh), Linear, Slack and Notion (the MCP servers `bough
// mcp` already reaches) and writes entities with their link truth —
// url, status, summary — plus the deterministic edges the design doc
// names (implements, authored, assigned, reviews, discusses, mentions,
// awaits, has_state). Every write cites a collector episode and is
// signed "collector"; no model is involved.
//
// `bough collect install` schedules it under launchd every few
// minutes. It must be a GUI launchd agent: the MCP tokens live in the
// login keychain, which is locked to an ssh session.
package collect

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/graph"
	"github.com/andreylukin/bough/plugins/mcp"
)

// Config is the row's config.
type Config struct {
	Me      string        // my email; default git user.email
	Every   time.Duration // launchd interval (install); default 10m
	Github  bool          // default true when gh is on PATH
	Linear  string        // MCP server name, "" = off; default linear-server
	Slack   string        // MCP server name; default slack
	Notion  string        // MCP server name; default notion
	Queries []string      // extra Slack search queries
	Days    int           // how far back sources are asked; default 14
}

func parseConfig(cfg map[string]any) (Config, error) {
	c := Config{Me: gitEmail(), Every: 10 * time.Minute, Github: true, Linear: "linear-server", Slack: "slack", Notion: "notion", Days: 14}
	if _, err := exec.LookPath("gh"); err != nil {
		c.Github = false
	}
	for k, v := range cfg {
		switch k {
		case "me":
			s, _ := v.(string)
			if !strings.Contains(s, "@") {
				return c, fmt.Errorf("collect: me must be an email")
			}
			c.Me = strings.ToLower(s)
		case "every":
			s, _ := v.(string)
			d, err := time.ParseDuration(s)
			if err != nil || d < time.Minute {
				return c, fmt.Errorf("collect: every must be a duration of at least 1m")
			}
			c.Every = d
		case "github":
			b, ok := v.(bool)
			if !ok {
				return c, fmt.Errorf("collect: github must be a bool")
			}
			c.Github = b
		case "linear", "slack", "notion":
			s, ok := v.(string)
			if !ok {
				return c, fmt.Errorf("collect: %s must be an MCP server name (\"\" for off)", k)
			}
			switch k {
			case "linear":
				c.Linear = s
			case "slack":
				c.Slack = s
			case "notion":
				c.Notion = s
			}
		case "queries":
			l, ok := v.([]any)
			if !ok {
				return c, fmt.Errorf("collect: queries must be a list")
			}
			for _, q := range l {
				if s, ok := q.(string); ok && s != "" {
					c.Queries = append(c.Queries, s)
				}
			}
		case "days":
			n, ok := v.(int)
			if !ok || n < 1 {
				return c, fmt.Errorf("collect: days must be a positive integer")
			}
			c.Days = n
		default:
			return c, fmt.Errorf("collect: unknown config key %q", k)
		}
	}
	return c, nil
}

func gitEmail() string {
	out, err := exec.Command("git", "config", "--get", "user.email").Output()
	if err != nil {
		return ""
	}
	return strings.ToLower(strings.TrimSpace(string(out)))
}

// Run is one collection: the store, my person entity, and the episode
// every write of this run cites.
type Run struct {
	St   *graph.Store
	Me   graph.Entity
	Ep   int64
	Now  int64
	Days int
	Log  func(format string, args ...any)
	// call is the MCP seam, replaced in tests.
	call func(server, tool, args string) (string, error)
	// gh is the GitHub CLI seam, replaced in tests.
	gh func(args ...string) ([]byte, error)
}

// Report counts one source's writes.
type Report struct {
	Source   string
	Entities int
	Edges    int
	Err      error
}

func (r Report) String() string {
	if r.Err != nil {
		return fmt.Sprintf("%-7s failed: %v", r.Source, r.Err)
	}
	return fmt.Sprintf("%-7s %d entities, %d edges", r.Source, r.Entities, r.Edges)
}

// NewRun opens a run over st for me, with a fresh episode.
func NewRun(st *graph.Store, me string) (*Run, error) {
	if me == "" {
		return nil, errors.New("collect: no `me` (config me: <email>, or git user.email)")
	}
	ep, err := st.Episode("collector", time.Now().UTC().Format(time.RFC3339))
	if err != nil {
		return nil, err
	}
	p, err := st.Upsert("person", strings.ToLower(me), "", "")
	if err != nil {
		return nil, err
	}
	return &Run{
		St: st, Me: p, Ep: ep, Now: time.Now().Unix(), Days: 14,
		Log:  func(string, ...any) {},
		call: mcp.Call,
		gh: func(args ...string) ([]byte, error) {
			cmd := exec.Command("gh", args...)
			cmd.Stderr = os.Stderr
			return cmd.Output()
		},
	}, nil
}

// --- shared write helpers: every edge here is observed, signed "collector".

func (r *Run) assert(src graph.Entity, rel string, dst graph.Entity, at int64, claim string) (bool, error) {
	if at == 0 {
		at = r.Now
	}
	_, created, err := r.St.AssertNew(src, rel, dst, r.Ep, "collector", graph.AssertOpts{ValidFrom: at, ObservedAt: at, Claim: claim})
	return created, err
}

// person resolves a person by email when known, else by a source
// login ("gh:bradley", "slack:U0B2L76PCLQ") with the login as an
// alias, so a later email can fold it. Me is always me.
func (r *Run) person(source, foreignID, email, name string) (graph.Entity, error) {
	email = strings.ToLower(strings.TrimSpace(email))
	key := email
	if key == "" {
		key = source + ":" + foreignID
	}
	if email != "" && email == r.Me.Key || foreignID != "" && r.isMe(source, foreignID) {
		key = r.Me.Key
	}
	e, err := r.St.Upsert("person", key, name, "")
	if err != nil {
		return e, err
	}
	if foreignID != "" {
		_ = r.St.Alias(e.ID, source, foreignID, "")
	}
	return e, nil
}

// isMe reports whether a source id was aliased to me.
func (r *Run) isMe(source, foreignID string) bool {
	id, err := r.St.AliasOwner(source, foreignID)
	return err == nil && id == r.Me.ID
}

// pr ensures a PR entity by "repo#n" key.
func (r *Run) pr(key, title, url string) (graph.Entity, error) {
	e, err := r.St.Upsert("pr", key, title, "")
	if err != nil {
		return e, err
	}
	if url != "" {
		return r.St.SetLink(e, graph.Link{URL: url})
	}
	return e, nil
}

// ticket ensures a ticket entity; the Linear collector fills the title.
func (r *Run) ticket(id string) (graph.Entity, error) {
	return r.St.Upsert("ticket", id, id, "")
}

// linkText records what free text refers to: src -discusses-> each PR
// and ticket named in it.
func (r *Run) linkText(src graph.Entity, text string, at int64) (int, error) {
	n := 0
	for _, t := range graph.Tickets(text) {
		te, err := r.ticket(t)
		if err != nil {
			return n, err
		}
		if ok, err := r.assert(src, "discusses", te, at, ""); err != nil {
			return n, err
		} else if ok {
			n++
		}
	}
	for _, p := range graph.PRLinks(text) {
		pe, err := r.pr(p, "", "")
		if err != nil {
			return n, err
		}
		if ok, err := r.assert(src, "discusses", pe, at, ""); err != nil {
			return n, err
		} else if ok {
			n++
		}
	}
	return n, nil
}

// jsonArgs encodes tool arguments without HTML escaping: "<@U…>" must
// reach Slack as typed.
func jsonArgs(v any) string {
	var b strings.Builder
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(v)
	return strings.TrimSpace(b.String())
}

// --- small parsing helpers shared by the MCP sources

// jsonIn finds the first JSON value (object or array) in an MCP text
// reply, which may wrap it in prose or a code fence.
func jsonIn(text string) string {
	text = strings.TrimSpace(text)
	if i := strings.Index(text, "```"); i >= 0 {
		rest := text[i+3:]
		if j := strings.Index(rest, "\n"); j >= 0 {
			rest = rest[j+1:]
		}
		if k := strings.LastIndex(rest, "```"); k >= 0 {
			rest = rest[:k]
		}
		text = strings.TrimSpace(rest)
	}
	i := strings.IndexAny(text, "{[")
	if i < 0 {
		return ""
	}
	return text[i:]
}

// str reads a string field, or a nested {name}/{title} object.
func str(m map[string]any, keys ...string) string {
	for _, k := range keys {
		switch v := m[k].(type) {
		case string:
			if v != "" {
				return v
			}
		case map[string]any:
			if s := str(v, "name", "title", "identifier", "login", "id"); s != "" {
				return s
			}
		}
	}
	return ""
}

// when parses the timestamps sources use (RFC3339, "2026-09-06",
// Slack's "1725600000.000100", epoch seconds).
func when(s string) int64 {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0
	}
	for _, layout := range []string{time.RFC3339Nano, time.RFC3339, "2006-01-02T15:04:05", "2006-01-02"} {
		if t, err := time.Parse(layout, s); err == nil {
			return t.Unix()
		}
	}
	if m := epochRe.FindStringSubmatch(s); m != nil {
		var n int64
		fmt.Sscanf(m[1], "%d", &n)
		if n > 1e12 {
			n /= 1000
		}
		return n
	}
	return 0
}

var epochRe = regexp.MustCompile(`^(\d{9,13})(?:\.\d+)?$`)

// status normalizes a source's state name: "In Progress" → in_progress,
// "Done " → done, "MERGED" → merged.
func status(s string) string {
	s = strings.ToLower(strings.TrimSpace(s))
	return strings.NewReplacer(" ", "_", "-", "_").Replace(s)
}

func home() string {
	h, _ := os.UserHomeDir()
	return h
}

func graphPath() string { return filepath.Join(home(), ".bough", "graph.db") }

type plugin struct{}

func init() {
	kernel.Register("collect", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "collect" }
func (plugin) Inject() []string { return nil }

// Apply validates the row; the work is the CLI's.
func (plugin) Apply(_ *kernel.Context, cfg map[string]any) error {
	_, err := parseConfig(cfg)
	return err
}
