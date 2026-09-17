package main

import (
	"strings"

	"github.com/andreylukin/bough/plugins/history"
)

// resumedModelSets are the /model swaps a session recorded, in order,
// as --set overrides: a resumed session (the web restarts its child on
// Esc) keeps the model it was switched to instead of bough.yml's.
func resumedModelSets(path string) []string {
	entries, err := history.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []string
	for _, e := range entries {
		if e.Kind != "model" {
			continue
		}
		vals, _ := e.Data["sets"].([]any)
		for _, v := range vals {
			if s, ok := v.(string); ok && !strings.HasPrefix(s, "history.") {
				out = append(out, s)
			}
		}
	}
	return out
}
