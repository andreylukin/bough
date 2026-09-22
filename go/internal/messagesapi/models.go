package messagesapi

import (
	"slices"
	"strings"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

// ModelSpec is what the Messages API needs to know about a model that
// models.dev does not carry: how it thinks, which effort levels it
// takes, how much it may write, and the betas it needs.
type ModelSpec struct {
	ID         string
	Thinking   string   // "adaptive" | "budget"
	Efforts    []string // accepted output_config.effort values; nil = effort not sent
	MaxOutput  int64
	Display    string // default display bough sends
	Betas      []anthropic.AnthropicBeta
	SystemMsgs bool // mid-conversation role:"system" accepted

	// Family is the table row the model matched ("claude-opus-5" for
	// "claude-opus-5-20260401"). Additive to the frozen shape: the
	// fallbacks rule names families, not ids.
	Family string
	// DefaultEffort is sent when the row has no effort set. Opus 5.5
	// defaults to medium server-side, one level below Opus 5, so a
	// session that never touched /think would silently think less
	// after an upgrade; it gets high instead.
	DefaultEffort string
}

var (
	allEfforts   = []string{"low", "medium", "high", "xhigh", "max"}
	effortsNoX   = []string{"low", "medium", "high", "max"}
	effortsThree = []string{"low", "medium", "high"}
)

// specs is keyed by id prefix; the longest matching prefix wins, so
// "claude-opus-5-5" is never read as "claude-opus-5".
var specs = []ModelSpec{
	{Family: "claude-opus-5-5", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "updates", DefaultEffort: "high"},
	{Family: "claude-fable-5-1", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "updates", SystemMsgs: true},
	{Family: "claude-fable-5", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "updates", SystemMsgs: true},
	{Family: "claude-opus-5", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "summarized", SystemMsgs: true},
	{Family: "claude-opus-4-8", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "summarized", SystemMsgs: true},
	{Family: "claude-opus-4-7", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "summarized"},
	{Family: "claude-sonnet-5", Thinking: "adaptive", Efforts: allEfforts, MaxOutput: 128000, Display: "summarized"},
	// 4.6 already defaults to summarized, and was never sent a display.
	{Family: "claude-opus-4-6", Thinking: "adaptive", Efforts: effortsNoX, MaxOutput: 128000},
	{Family: "claude-sonnet-4-6", Thinking: "adaptive", Efforts: effortsNoX, MaxOutput: 128000},
	{Family: "claude-opus-4-5", Thinking: "budget", Efforts: effortsThree, MaxOutput: 64000},
	{Family: "claude-haiku-4-5", Thinking: "budget", MaxOutput: 64000, Betas: []anthropic.AnthropicBeta{anthropic.AnthropicBetaInterleavedThinking2025_05_14}},
	{Family: "claude-sonnet-4-5", Thinking: "budget", MaxOutput: 64000, Betas: []anthropic.AnthropicBeta{anthropic.AnthropicBetaInterleavedThinking2025_05_14}},
}

// defaultFamily answers for an id the table does not know: new models
// arrive faster than this table, and the newest generally behaves like
// the current Opus.
const defaultFamily = "claude-opus-5"

// Spec returns the spec for a model id.
func Spec(model string) ModelSpec {
	id := strings.ToLower(strings.TrimSpace(model))
	best := -1
	for i, s := range specs {
		// Whole dash-separated parts only, longest first: a dated
		// snapshot takes its family's row, and "claude-opus-5-5" is
		// never read as "claude-opus-5".
		if (id == s.Family || strings.HasPrefix(id, s.Family+"-")) && (best < 0 || len(s.Family) > len(specs[best].Family)) {
			best = i
		}
	}
	var s ModelSpec
	if best < 0 {
		for _, c := range specs {
			if c.Family == defaultFamily {
				s = c
			}
		}
	} else {
		s = specs[best]
	}
	s.ID = model
	s.Efforts = slices.Clone(s.Efforts)
	s.Betas = slices.Clone(s.Betas)
	return s
}

// fallbackFamilies get server-side fallbacks under fallbacks: auto. The
// claude-api skill prescribes them for exactly these two, whose safety
// classifiers can decline a benign coding request.
var fallbackFamilies = []string{"claude-opus-5", "claude-fable-5-1"}

// EffortFor maps a bough effort level to what the request carries for
// this model: "" means no effort is sent (and, on a budget model, no
// thinking at all).
func EffortFor(level string, s ModelSpec) ullm.ReasoningEffort {
	switch level {
	case "":
		if s.Thinking == "adaptive" && s.DefaultEffort != "" {
			return ullm.ReasoningEffort(s.DefaultEffort)
		}
		return ""
	case "off":
		if s.Thinking == "adaptive" {
			// No 5.x model can switch thinking off; low is the closest
			// thing, and disabled is a 400 on the newest ones.
			return ullm.ReasoningEffortLow
		}
		return ""
	}
	e := ullm.ReasoningEffort(level)
	if !e.Valid() {
		return ""
	}
	return e
}

// Clamp returns the highest accepted level at or below want, or the
// lowest accepted one when none is below. xhigh on Opus 4.6 becomes
// high, not max: asking for more than the model offers should not buy
// the most expensive level.
func Clamp(want string, accepted []string) string {
	if len(accepted) == 0 || slices.Contains(accepted, want) {
		return want
	}
	rank := slices.Index(allEfforts, want)
	if rank < 0 {
		return ""
	}
	out := ""
	for _, a := range accepted {
		if r := slices.Index(allEfforts, a); r >= 0 && r <= rank && (out == "" || r > slices.Index(allEfforts, out)) {
			out = a
		}
	}
	if out == "" {
		out = accepted[0]
	}
	return out
}

// budgets is the thinking budget per effort on the models that still
// take budget_tokens.
var budgets = map[ullm.ReasoningEffort]int64{
	ullm.ReasoningEffortLow:    2048,
	ullm.ReasoningEffortMedium: 8192,
	ullm.ReasoningEffortHigh:   16384,
	ullm.ReasoningEffortXHigh:  32768,
	ullm.ReasoningEffortMax:    32768,
}
