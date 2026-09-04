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
// refresh is started. Prices move on the scale of months.
const maxAge = 7 * 24 * time.Hour

// Model is what bough needs to know about one model.
type Model struct {
	Input   float64  `json:"i,omitempty"` // $ per million input tokens
	Output  float64  `json:"o,omitempty"` // $ per million output tokens
	Tiers   []Tier   `json:"t,omitempty"` // price steps by context size
	Context int      `json:"c,omitempty"` // context window in tokens
	Release string   `json:"r,omitempty"` // release date, "YYYY-MM-DD"
	Efforts []string `json:"e,omitempty"` // reasoning levels it accepts
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
	inRate, outRate := m.Input, m.Output
	for _, t := range m.Tiers {
		if in >= t.Over && t.Input > 0 {
			inRate, outRate = t.Input, t.Output
		}
	}
	return float64(in)*inRate/1e6 + float64(out)*outRate/1e6
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
	if p := cachePath(); p != "" {
		if b, err := json.Marshal(out); err == nil {
			_ = os.MkdirAll(filepath.Dir(p), 0o755)
			tmp := p + ".tmp"
			if os.WriteFile(tmp, b, 0o644) == nil {
				_ = os.Rename(tmp, p)
			}
		}
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

// decode reads a cached catalogue (already trimmed).
func decode(b []byte) (Catalogue, error) {
	var c Catalogue
	if err := json.Unmarshal(b, &c); err != nil {
		return nil, err
	}
	return c, nil
}

// upstream is the slice of models.dev's api.json Trim reads.
type upstream map[string]struct {
	Models map[string]struct {
		Cost *struct {
			Input  float64 `json:"input"`
			Output float64 `json:"output"`
			Tiers  []struct {
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
