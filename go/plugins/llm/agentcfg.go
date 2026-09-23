package llm

// The engine seam's configuration and bookkeeping (see agent.go): the
// llm-anthropic engine keys, the effort fitting both engines share, and
// folding an engine response's usage into a row's tally. Kept apart
// from agent.go so the loop's rows build where the harness does not.

import (
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/messagesapi"
	"github.com/andreylukin/bough/internal/models"
)

// agentAnthropic is llm-anthropic's engine-only config. The loop never
// reads it.
type agentAnthropic struct {
	cacheTTL    string // "1h" | "5m"
	display     string // "auto" | "summarized" | "omitted" | "updates"
	fallbacks   string // "auto" | "off"
	binding     string // "" | "drop_block" | "error"
	maxAttempts int
	idle        time.Duration
	base        string // tests point the client at a local server
}

func parseAgentAnthropic(cfg map[string]any) (agentAnthropic, error) {
	out := agentAnthropic{cacheTTL: "1h", display: "auto", fallbacks: "auto"}
	var err error
	if out.cacheTTL, err = oneOf(cfg, "llm-anthropic", "cache_ttl", out.cacheTTL, "1h", "5m"); err != nil {
		return out, err
	}
	if out.display, err = oneOf(cfg, "llm-anthropic", "thinking_display", out.display, "auto", "summarized", "omitted", "updates"); err != nil {
		return out, err
	}
	if out.fallbacks, err = oneOf(cfg, "llm-anthropic", "fallbacks", out.fallbacks, "auto", "off"); err != nil {
		return out, err
	}
	if out.binding, err = oneOf(cfg, "llm-anthropic", "block_binding", "", "", "drop_block", "error"); err != nil {
		return out, err
	}
	if out.maxAttempts, err = attempts(cfg, "llm-anthropic"); err != nil {
		return out, err
	}
	if v, ok := cfg["idle_timeout"]; ok {
		d, err := duration(v)
		if err != nil || d < 0 {
			return out, fmt.Errorf("llm-anthropic: idle_timeout must be a duration like 5m, got %v", v)
		}
		out.idle = d
	}
	return out, nil
}

func oneOf(cfg map[string]any, row, key, def string, allowed ...string) (string, error) {
	v, ok := cfg[key]
	if !ok {
		return def, nil
	}
	s, ok := v.(string)
	for _, a := range allowed {
		if ok && s == a {
			return s, nil
		}
	}
	var names []string
	for _, a := range allowed {
		if a != "" {
			names = append(names, a)
		}
	}
	return "", fmt.Errorf("%s: %s must be %s, got %v", row, key, strings.Join(names, " or "), v)
}

func attempts(cfg map[string]any, row string) (int, error) {
	v, ok := cfg["max_attempts"]
	if !ok {
		return 0, nil
	}
	n, ok := v.(int)
	if !ok || n < 0 {
		return 0, fmt.Errorf("%s: max_attempts must be a non-negative integer, got %v", row, v)
	}
	return n, nil
}

func duration(v any) (time.Duration, error) {
	switch d := v.(type) {
	case string:
		return time.ParseDuration(d)
	case int:
		return time.Duration(d) * time.Second, nil
	}
	return 0, fmt.Errorf("not a duration: %v", v)
}

// clampEffort fits a bough level to what the model's catalogue entry
// accepts, when it lists any: max on a model that stops at xhigh asks
// for xhigh rather than a 400. Unknown models pass through.
//
// off is the Messages adapter's to map (it can disable thinking). The
// Responses API has no "none", so there off asks for low; on a model
// whose catalogue has no low (gpt-5-pro takes only high) that is a 400
// on every request, so off becomes the least level the model lists.
func clampEffort(plugin, model, level string) string {
	if level == "" || (level == "off" && plugin == "llm-anthropic") {
		return level
	}
	m, ok := models.Lookup(plugin, model)
	if !ok || len(m.Efforts) == 0 {
		return level
	}
	if level == "off" {
		if slices.Contains(m.Efforts, "low") {
			return level
		}
		return messagesapi.Clamp("low", m.Efforts)
	}
	if c := messagesapi.Clamp(level, m.Efforts); c != "" {
		return c
	}
	return level
}

// loopLevel is the level the loop's OpenAI, OpenRouter and Cerebras
// paths send. They send every other level as they always have; max is
// newer than they are, so it is fitted to the model's catalogue entry,
// and on a model the catalogue does not know it asks for xhigh, the
// most any of those paths sent before max existed.
func loopLevel(plugin, model, level string) string {
	if level != EffortMax {
		return level
	}
	if m, ok := models.Lookup(plugin, model); ok && len(m.Efforts) > 0 {
		if c := messagesapi.Clamp(level, m.Efforts); c != "" {
			return c
		}
	}
	return "xhigh"
}

// loopEffort is the output_config.effort the loop's Anthropic path
// sends: nothing unless a level was set, and nothing on a model that
// takes no effort.
func loopEffort(model, level string) string {
	if level == "" {
		return ""
	}
	spec := messagesapi.Spec(model)
	e := messagesapi.EffortFor(level, spec)
	if e == "" || spec.Efforts == nil {
		return ""
	}
	return messagesapi.Clamp(string(e), spec.Efforts)
}

// addAgentUsage folds one engine response into a row's tally, so the
// status bar, /cost, the cost row and done.usage read the same numbers
// they read on the loop. InputTokens stays inclusive of the cache.
func addAgentUsage(u *Usage, r ullm.Usage) {
	u.InputTokens += int(r.InputTokens)
	u.OutputTokens += int(r.OutputTokens)
	if r.InputTokens > 0 {
		u.LastInputTokens = int(r.InputTokens)
	}
	u.CacheReadTokens += int(r.CachedInputTokens)
	u.CacheCreationTokens += int(r.CacheWriteInputTokens)
	if len(r.Raw) == 0 {
		return
	}
	var raw struct {
		CacheCreation struct {
			OneHour int `json:"ephemeral_1h_input_tokens"`
		} `json:"cache_creation"`
		Cost *float64 `json:"cost"`
	}
	if json.Unmarshal(r.Raw, &raw) != nil {
		return
	}
	u.CacheWrite1hTokens += raw.CacheCreation.OneHour
	if raw.Cost != nil {
		// OpenRouter prices every response itself; the Anthropic and
		// OpenAI tallies stay unpriced and the cost row prices them.
		u.Cost += *raw.Cost
		u.Priced = true
	}
}

// addFallbackUsage folds in what an Anthropic server-side fallback bills
// beyond the top-level usage, which covers only the attempt that
// produced the message: a declined attempt's partial output is only in
// usage.iterations, and every attempt bills at the rates of the model
// that ran it. With sticky routing a Fable row can be served by Opus
// for an hour, so pricing it all at the row's model shows twice the
// real cost. The difference goes to FallbackCost, which the cost row
// adds to its row-model pricing; an unpriced model adds nothing.
func addFallbackUsage(u *Usage, r ullm.Usage, row string) {
	if len(r.Raw) == 0 {
		return
	}
	var raw struct {
		Iterations []struct {
			Type          string `json:"type"`
			Model         string `json:"model"`
			In            int    `json:"input_tokens"`
			Out           int    `json:"output_tokens"`
			Read          int    `json:"cache_read_input_tokens"`
			Write         int    `json:"cache_creation_input_tokens"`
			CacheCreation struct {
				OneHour int `json:"ephemeral_1h_input_tokens"`
			} `json:"cache_creation"`
		} `json:"iterations"`
	}
	if json.Unmarshal(r.Raw, &raw) != nil {
		return
	}
	rowM, rowOK := models.Lookup("llm-anthropic", row)
	last := len(raw.Iterations) - 1
	for i, it := range raw.Iterations {
		in := it.In + it.Read + it.Write
		if i < last {
			// The last attempt is the top-level usage, already counted.
			// One declined before any output is reported, not billed.
			if it.Out == 0 {
				continue
			}
			u.InputTokens += in
			u.OutputTokens += it.Out
			u.CacheReadTokens += it.Read
			u.CacheCreationTokens += it.Write
			u.CacheWrite1hTokens += it.CacheCreation.OneHour
		}
		if it.Model == "" || it.Model == row || !rowOK {
			continue
		}
		if m, ok := models.Lookup("llm-anthropic", it.Model); ok && m.Input > 0 {
			u.FallbackCost += m.CostCached1h(in, it.Out, it.Read, it.Write, it.CacheCreation.OneHour) -
				rowM.CostCached1h(in, it.Out, it.Read, it.Write, it.CacheCreation.OneHour)
		}
	}
}

