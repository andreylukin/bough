// Package session is the "session" plugin: one place a plugin reads
// what is true about the running session — its id and title, the
// model and provider answering, what it has spent, how full the
// context is, how many turns it has taken.
//
// Those facts already exist, but each in a different row (history,
// llm, cost, session-title) behind a different seam, and a plugin that
// wanted two of them had to know all of that. This row Provides
// "session" (a *Service): Info() is the facts as a struct for Go rows,
// Map() the same as a plain map for init.js's bough.session(). Both
// resolve at call time, never at mount, so the row can sit anywhere in
// the config and still see /model swaps and a resumed session.
package session

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Usage is what the session has spent so far.
type Usage struct {
	In, Out    int     // tokens sent and received, whole session
	CacheRead  int     // input read back from the provider's prompt cache
	CacheWrite int     // input written to it
	LastIn     int     // the last request's input size: what the next turn starts from
	Cost       float64 // USD; meaningful when Priced
	Priced     bool
}

// Info is the session as a plugin sees it. Zero values mean unknown
// (no such row mounted, or nothing has happened yet).
type Info struct {
	ID       string // the history file's base name
	Title    string // the session-title row's name for it, once there is one
	Model    string // the llm row's model id, as configured (provider prefix and all)
	Provider string // the llm row's plugin: llm-anthropic, llm-openrouter, ...
	Cwd      string
	Started  time.Time // the first history entry
	Turns    int       // completed turns (done entries)
	Usage    Usage
	// ContextLimit is the model's window in tokens when known;
	// ContextPct is how much of it the next turn starts with.
	ContextLimit int
	ContextPct   float64
}

// The seams, each optional.
type entries interface{ Entries() []history.Entry }
type pather interface{ Path() string }
type limiter interface{ ContextLimit() int }

// Service answers Info from whatever rows are mounted now.
type Service struct {
	ctx *kernel.Context
}

// Info is the session right now.
func (s *Service) Info() Info {
	var info Info
	if h, err := kernel.Get[pather](s.ctx, "history"); err == nil && h.Path() != "" {
		info.ID = strings.TrimSuffix(filepath.Base(h.Path()), filepath.Ext(h.Path()))
	}
	if h, err := kernel.Get[entries](s.ctx, "history"); err == nil {
		for i, e := range h.Entries() {
			if i == 0 {
				info.Started = e.At
			}
			switch e.Kind {
			case "done":
				info.Turns++
			case "title":
				info.Title, _ = e.Data["text"].(string)
			case "meta":
				if cwd, ok := e.Data["cwd"].(string); ok && cwd != "" {
					info.Cwd = cwd
				}
			}
		}
	}
	if info.Cwd == "" {
		info.Cwd, _ = os.Getwd()
	}
	if m, err := kernel.Get[llm.Modeler](s.ctx, "llm"); err == nil {
		info.Model = m.Model()
	}
	for _, r := range s.ctx.Desired() {
		if r.ID == "llm" {
			info.Provider = r.Plugin
		}
	}
	// The cost row's tally (priced, with the session's history folded
	// in) when there is one; the llm's own count otherwise.
	var u llm.Usage
	if rep, err := kernel.Get[llm.UsageReporter](s.ctx, "usage"); err == nil {
		u = rep.Usage()
	} else if rep, err := kernel.Get[llm.UsageReporter](s.ctx, "llm"); err == nil {
		u = rep.Usage()
	}
	info.Usage = Usage{In: u.InputTokens, Out: u.OutputTokens, CacheRead: u.CacheReadTokens, CacheWrite: u.CacheCreationTokens, LastIn: u.LastInputTokens, Cost: u.Cost, Priced: u.Priced}
	if l, err := kernel.Get[limiter](s.ctx, "usage"); err == nil {
		info.ContextLimit = l.ContextLimit()
	}
	if info.ContextLimit > 0 {
		info.ContextPct = 100 * float64(u.LastInputTokens) / float64(info.ContextLimit)
	}
	return info
}

// Map is Info as init.js sees it: snake_case keys, a nested usage
// object, times as RFC 3339 strings.
func (s *Service) Map() map[string]any {
	i := s.Info()
	started := ""
	if !i.Started.IsZero() {
		started = i.Started.Format(time.RFC3339)
	}
	return map[string]any{
		"id":            i.ID,
		"title":         i.Title,
		"model":         i.Model,
		"provider":      i.Provider,
		"cwd":           i.Cwd,
		"started":       started,
		"turns":         i.Turns,
		"context_limit": i.ContextLimit,
		"context_pct":   i.ContextPct,
		"usage": map[string]any{
			"in":          i.Usage.In,
			"out":         i.Usage.Out,
			"cache_read":  i.Usage.CacheRead,
			"cache_write": i.Usage.CacheWrite,
			"last_in":     i.Usage.LastIn,
			"cost":        i.Usage.Cost,
			"priced":      i.Usage.Priced,
		},
	}
}

type plugin struct{}

func init() {
	kernel.Register("session", func() kernel.Plugin { return plugin{} })
}

func (plugin) Name() string     { return "session" }
func (plugin) Inject() []string { return nil }

func (plugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	for k := range cfg {
		return fmt.Errorf("session: unknown config key %q", k)
	}
	ctx.Provide("session", &Service{ctx: ctx})
	return nil
}
