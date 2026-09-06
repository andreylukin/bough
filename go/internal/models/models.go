// Package models is the model catalogue: what every provider's models
// cost, how much context they hold, and which reasoning levels they
// take. bough used to hard-code a few dozen prices and context limits,
// which meant an unknown model showed no cost and no "% ctx" — and a
// tiered price (gpt-5.6-sol doubles above 272k tokens) was charged flat.
//
// The data is models.dev (https://models.dev/api.json), the open
// database opencode maintains: ~7,500 provider-model rows, refreshed
// here once a week into ~/.bough/models.json. A trimmed snapshot of the
// four providers bough ships plugins for is embedded, so a fresh
// install with no network still prices what it runs.
//
// The snapshot is regenerated from the live API:
//
//	curl -s https://models.dev/api.json > /tmp/api.json
//	go run ./internal/models/gen -in /tmp/api.json -out internal/models/snapshot.json.gz
package models

import (
	"compress/gzip"
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

//go:embed snapshot.json.gz
var snapshot []byte

// URL is where the live catalogue comes from.
const URL = "https://models.dev/api.json"

// maxAge is how stale a cached catalogue may get before a background
// refresh is started. Prices move on the scale of months, but MODELS
// arrive weekly and a week-old cache does not list the one someone is
// asking for by name — openai/gpt-6-astra was on models.dev and absent
// here for exactly that reason. Refresh returns it on demand when
// even a day is too long.
const maxAge = 24 * time.Hour

// cacheVersion is the shape of ~/.bough/models.json. A cache written
// by an older bough is missing whatever fields were added since — the
// prompt-cache rates landed this way, and a week-old cache silently
// priced every cached token at the full rate. A cache from another
// version is ignored and refetched.
const cacheVersion = 2

// cacheFile is what the cache holds: the catalogue plus its version.
type cacheFile struct {
	Version   int       `json:"v"`
	Catalogue Catalogue `json:"c"`
}

// Model is what bough needs to know about one model.
type Model struct {
	Input  float64 `json:"i,omitempty"` // $ per million input tokens
	Output float64 `json:"o,omitempty"` // $ per million output tokens
	// CacheRead/CacheWrite are the prompt-cache rates: a cached read
	// is a tenth of the input price on Anthropic, a write a little
	// over. Without them a cached turn is billed as if nothing were
	// cached, which is the opposite of the truth.
	CacheRead  float64  `json:"cr,omitempty"`
	CacheWrite float64  `json:"cw,omitempty"`
	Tiers      []Tier   `json:"t,omitempty"` // price steps by context size
	Context    int      `json:"c,omitempty"` // context window in tokens
	Release    string   `json:"r,omitempty"` // release date, "YYYY-MM-DD"
	Efforts    []string `json:"e,omitempty"` // reasoning levels it accepts
}

// Tier is a price that applies once a request's input passes Over
// tokens (OpenAI's long-context surcharge).
type Tier struct {
	Over   int     `json:"over"`
	Input  float64 `json:"i"`
	Output float64 `json:"o"`
}

// Catalogue is provider id -> model id -> Model.
type Catalogue map[string]map[string]Model

// Provider maps an llm-* plugin name to its models.dev provider id.
// A plugin with no entry has no catalogue, which is not an error.
func Provider(plugin string) string {
	switch plugin {
	case "llm-anthropic":
		return "anthropic"
	case "llm-openai":
		return "openai"
	case "llm-openrouter":
		return "openrouter"
	case "llm-cerebras":
		return "cerebras"
	}
	return ""
}

var (
	once   sync.Once
	mu     sync.RWMutex
	loaded Catalogue
)

// Load returns the catalogue, reading the cache (or the embedded
// snapshot) once and starting a background refresh when the cache is
// missing or stale. Never blocks on the network.
func Load() Catalogue {
	once.Do(func() {
		c, age := fromCache()
		if c == nil {
			c = fromSnapshot()
		}
		mu.Lock()
		loaded = c
		mu.Unlock()
		if age < 0 || age > maxAge {
			go refresh()
		}
	})
	mu.RLock()
	defer mu.RUnlock()
	return loaded
}

// Lookup finds one model. plugin is the llm-* plugin name; an id is
// matched exactly, then by its longest catalogue prefix, so a dated
// snapshot ("claude-haiku-4-5-20251001") prices like its base model.
func Lookup(plugin, model string) (Model, bool) {
	models, ok := Load()[Provider(plugin)]
	if !ok {
		return Model{}, false
	}
	id := strings.ToLower(strings.TrimSpace(model))
	if m, ok := models[id]; ok {
		return m, true
	}
	best, found := "", false
	var out Model
	for k, m := range models {
		if strings.HasPrefix(id, strings.ToLower(k)+"-") && len(k) > len(best) {
			best, out, found = k, m, true
		}
	}
	return out, found
}

// Cost prices a request. Tiers apply by INPUT size: the highest tier
// the request passes wins, which is how OpenAI's long-context
// surcharge works.
func (m Model) Cost(in, out int) float64 {
	return m.CostCached(in, out, 0, 0)
}

// CostCached prices a request whose input was partly served from the
// prompt cache. read and write are counted inside in (the providers
// report them that way), so they are subtracted before the full rate
// applies and charged at their own. A model with no cache rates falls
// back to the input rate, which is what was charged before caching.
func (m Model) CostCached(in, out, read, write int) float64 {
	inRate, outRate := m.Input, m.Output
	for _, t := range m.Tiers {
		if in >= t.Over && t.Input > 0 {
			inRate, outRate = t.Input, t.Output
		}
	}
	readRate, writeRate := m.CacheRead, m.CacheWrite
	if readRate == 0 {
		readRate = inRate
	}
	if writeRate == 0 {
		writeRate = inRate
	}
	full := in - read - write
	if full < 0 {
		full = 0
	}
	return (float64(full)*inRate + float64(read)*readRate + float64(write)*writeRate + float64(out)*outRate) / 1e6
}

// List is a provider's model ids, newest first (undated ones last,
// alphabetically). n <= 0 returns all of them.
func List(plugin string, n int) []string {
	models := Load()[Provider(plugin)]
	ids := make([]string, 0, len(models))
	for id := range models {
		ids = append(ids, id)
	}
	sort.Slice(ids, func(a, b int) bool {
		ra, rb := models[ids[a]].Release, models[ids[b]].Release
		if ra != rb {
			return ra > rb // "" sorts last
		}
		return ids[a] < ids[b]
	})
	if n > 0 && len(ids) > n {
		ids = ids[:n]
	}
	return ids
}

// cachePath is ~/.bough/models.json; "" when there is no home dir.
func cachePath() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".bough", "models.json")
}

// fromCache reads the cached catalogue and its age; age is -1 when
// there is no usable cache.
func fromCache() (Catalogue, time.Duration) {
	p := cachePath()
	if p == "" {
		return nil, -1
	}
	st, err := os.Stat(p)
	if err != nil {
		return nil, -1
	}
	b, err := os.ReadFile(p)
	if err != nil {
		return nil, -1
	}
	c, err := decode(b)
	if err != nil || len(c) == 0 {
		return nil, -1
	}
	return c, time.Since(st.ModTime())
}

func fromSnapshot() Catalogue {
	zr, err := gzip.NewReader(strings.NewReader(string(snapshot)))
	if err != nil {
		return Catalogue{}
	}
	defer zr.Close()
	b, err := io.ReadAll(zr)
	if err != nil {
		return Catalogue{}
	}
	var c Catalogue
	if err := json.Unmarshal(b, &c); err != nil {
		return Catalogue{}
	}
	return c
}

// refresh fetches the live catalogue and replaces the cache. Failure
// is silent: the snapshot already answered.
func refresh() {
	c, err := fetch(context.Background(), URL)
	if err != nil || len(c) == 0 {
		return
	}
	mu.Lock()
	loaded = merge(loaded, c)
	out := loaded
	mu.Unlock()
	writeCache(out)
}

// writeCache stores the catalogue, atomically. Failure is silent: the
// in-memory copy already answered.
func writeCache(out Catalogue) {
	p := cachePath()
	if p == "" {
		return
	}
	b, err := json.Marshal(cacheFile{Version: cacheVersion, Catalogue: out})
	if err != nil {
		return
	}
	_ = os.MkdirAll(filepath.Dir(p), 0o755)
	tmp := p + ".tmp"
	if os.WriteFile(tmp, b, 0o644) == nil {
		_ = os.Rename(tmp, p)
	}
}

// merge keeps the snapshot's providers when the live copy lacks them.
func merge(old, live Catalogue) Catalogue {
	out := Catalogue{}
	for p, m := range old {
		out[p] = m
	}
	for p, m := range live {
		if len(m) > 0 {
			out[p] = m
		}
	}
	return out
}

// fetch reads models.dev and trims it to what bough uses. Exported
// shape (decode) is the same as the snapshot's, so a cache written by
// an older bough still parses.
func fetch(ctx context.Context, url string) (Catalogue, error) {
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, errors.New("models: " + resp.Status)
	}
	b, err := io.ReadAll(io.LimitReader(resp.Body, 32<<20))
	if err != nil {
		return nil, err
	}
	return Trim(b)
}

// decode reads a cached catalogue (already trimmed). A file from a
// different cacheVersion is refused, so it is refetched rather than
// answering with fields it never had.
func decode(b []byte) (Catalogue, error) {
	var f cacheFile
	if err := json.Unmarshal(b, &f); err != nil {
		return nil, err
	}
	if f.Version != cacheVersion {
		return nil, errors.New("models: cache written by another version")
	}
	return f.Catalogue, nil
}

// upstream is the slice of models.dev's api.json Trim reads.
type upstream map[string]struct {
	Models map[string]struct {
		Cost *struct {
			Input      float64 `json:"input"`
			Output     float64 `json:"output"`
			CacheRead  float64 `json:"cache_read"`
			CacheWrite float64 `json:"cache_write"`
			Tiers      []struct {
				Input  float64 `json:"input"`
				Output float64 `json:"output"`
				Tier   struct {
					Size int `json:"size"`
				} `json:"tier"`
			} `json:"tiers"`
		} `json:"cost"`
		Limit *struct {
			Context int `json:"context"`
		} `json:"limit"`
		Release          string `json:"release_date"`
		ReasoningOptions []struct {
			Type   string   `json:"type"`
			Values []string `json:"values"`
		} `json:"reasoning_options"`
	} `json:"models"`
}

// Trim converts models.dev's api.json to the catalogue, keeping only
// the providers bough has plugins for (the full file is 4 MB and 213
// providers, almost all of them resellers).
func Trim(b []byte) (Catalogue, error) {
	var up upstream
	if err := json.Unmarshal(b, &up); err != nil {
		return nil, err
	}
	out := Catalogue{}
	for _, plugin := range []string{"llm-anthropic", "llm-openai", "llm-openrouter", "llm-cerebras"} {
		pid := Provider(plugin)
		p, ok := up[pid]
		if !ok {
			continue
		}
		ms := map[string]Model{}
		for id, m := range p.Models {
			e := Model{Release: m.Release}
			if m.Cost != nil {
				e.Input, e.Output = m.Cost.Input, m.Cost.Output
				e.CacheRead, e.CacheWrite = m.Cost.CacheRead, m.Cost.CacheWrite
				for _, t := range m.Cost.Tiers {
					if t.Tier.Size > 0 {
						e.Tiers = append(e.Tiers, Tier{Over: t.Tier.Size, Input: t.Input, Output: t.Output})
					}
				}
			}
			if m.Limit != nil {
				e.Context = m.Limit.Context
			}
			for _, r := range m.ReasoningOptions {
				if r.Type == "effort" && len(r.Values) > 0 {
					e.Efforts = r.Values
				}
			}
			if e.Input > 0 || e.Context > 0 {
				ms[id] = e
			}
		}
		if len(ms) > 0 {
			out[pid] = ms
		}
	}
	return out, nil
}

// Refresh refetches the catalogue now and reports how many models each
// provider ended up with. Load's refresh is a background job on a
// timer; this is the one to call when a model is missing and the
// answer "wait until tomorrow" is not one.
func Refresh() (Catalogue, error) {
	c, err := fetch(context.Background(), URL)
	if err != nil {
		return nil, err
	}
	if len(c) == 0 {
		return nil, errors.New("models: the catalogue came back empty")
	}
	mu.Lock()
	loaded = merge(loaded, c)
	out := loaded
	mu.Unlock()
	writeCache(out)
	return out, nil
}

// Search returns every model whose id contains q, case-insensitively,
// as "provider/id" pairs sorted by provider then id. A catalogue of 359
// OpenRouter models is not something to read; it is something to search.
func Search(q string) []Match {
	q = strings.ToLower(strings.TrimSpace(q))
	var out []Match
	for prov, ms := range Load() {
		for id, m := range ms {
			if q == "" || strings.Contains(strings.ToLower(id), q) {
				out = append(out, Match{Provider: prov, ID: id, Model: m})
			}
		}
	}
	sort.Slice(out, func(a, b int) bool {
		if out[a].Provider != out[b].Provider {
			return out[a].Provider < out[b].Provider
		}
		// Newest first within a provider, undated last.
		if out[a].Release != out[b].Release {
			return out[a].Release > out[b].Release
		}
		return out[a].ID < out[b].ID
	})
	return out
}

// Match is one search hit.
type Match struct {
	Provider string // models.dev provider id ("openrouter")
	ID       string // the model id to pass to /model
	Model
}

// Plugin is the llm-* plugin name for a models.dev provider id, the
// inverse of Provider; "" when bough has no plugin for it.
func Plugin(provider string) string {
	for _, p := range []string{"llm-anthropic", "llm-openai", "llm-openrouter", "llm-cerebras"} {
		if Provider(p) == provider {
			return p
		}
	}
	return ""
}
