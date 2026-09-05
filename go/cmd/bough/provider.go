package main

// Choosing a provider for a first run.
//
// The embedded default config names llm-anthropic, so someone whose
// only key is OPENROUTER_API_KEY used to launch bough, be told
// "ANTHROPIC_API_KEY is not set", and have to learn the config format
// before sending a single message — while holding a key that works.
// The README says any of four keys will do, and now that is true.
//
// This applies ONLY to the embedded default's llm row. A bough.yml
// that overrides the llm row is the user's own statement of which
// provider to use and is never second-guessed: an explicit row that
// cannot run is an error worth seeing, not something to route around.

import (
	"os"

	"github.com/andreylukin/bough/kernel"
)

// providerChoice is one candidate for the default llm row, in
// preference order. The model is the provider's flagship at the time
// of writing; /model changes it, and the choice is only ever a
// starting point.
type providerChoice struct {
	env    string
	plugin string
	model  string
}

var providerChoices = []providerChoice{
	{"ANTHROPIC_API_KEY", "llm-anthropic", "claude-sonnet-5"},
	{"OPENROUTER_API_KEY", "llm-openrouter", "anthropic/claude-sonnet-5"},
	{"OPENAI_API_KEY", "llm-openai", "gpt-5.6"},
	{"CEREBRAS_API_KEY", "llm-cerebras", "gpt-oss-120b"},
}

// pickProvider rewrites the embedded default's llm row to the first
// provider whose key is set. It returns the rows unchanged when the
// row already matches a key that is present, when no key is set at all
// (the missing-key notice is the right answer then, and it names the
// provider the config asked for), or when the tree has no plain llm
// row to speak of.
//
// Only the "llm" service row is touched. An llm-small row is an opt-in
// the user configured deliberately.
func pickProvider(rows []kernel.Row) ([]kernel.Row, string) {
	i := -1
	for n, r := range rows {
		if r.ID == "llm" {
			i = n
			break
		}
	}
	if i < 0 {
		return rows, ""
	}
	// Already runnable: whatever the default names, its key is here.
	for _, c := range providerChoices {
		if rows[i].Plugin == c.plugin {
			if os.Getenv(c.env) != "" {
				return rows, ""
			}
			break
		}
	}
	for _, c := range providerChoices {
		if os.Getenv(c.env) == "" || rows[i].Plugin == c.plugin {
			continue
		}
		out := make([]kernel.Row, len(rows))
		copy(out, rows)
		cfg := map[string]any{}
		for k, v := range out[i].Config {
			cfg[k] = v
		}
		cfg["model"] = c.model
		out[i].Plugin = c.plugin
		out[i].Config = cfg
		return out, c.plugin + " (" + c.model + "), chosen because " + c.env + " is set"
	}
	return rows, ""
}
