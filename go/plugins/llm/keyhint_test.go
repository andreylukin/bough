package llm

import (
	"strings"
	"testing"
)

// A rejected key names the variable and the command that replaces it,
// for every HTTP provider.
func TestKeyRejectedNamesTheFix(t *testing.T) {
	t.Parallel()
	body := []byte(`{"error":{"message":"invalid key"}}`)
	for _, tc := range []struct {
		name string
		err  error
		want []string
	}{
		{"openai", openaiErr(401, "m", body), []string{"OPENAI_API_KEY was rejected", "invalid key", "/connect openai <key>"}},
		{"openrouter", openrouterErr(401, "m", body), []string{"OPENROUTER_API_KEY was rejected", "/connect openrouter <key>"}},
		{"cerebras", cerebrasErr(401, "m", body), []string{"CEREBRAS_API_KEY was rejected", "/connect cerebras <key>"}},
		{"no body", openaiErr(401, "m", nil), []string{"HTTP 401: Unauthorized"}},
	} {
		for _, w := range tc.want {
			if !strings.Contains(tc.err.Error(), w) {
				t.Errorf("%s: %q lacks %q", tc.name, tc.err, w)
			}
		}
	}
	if err := openaiErr(500, "m", body); strings.Contains(err.Error(), "rejected") {
		t.Errorf("a 500 is not a rejected key: %v", err)
	}
}
