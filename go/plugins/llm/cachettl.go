package llm

import (
	"regexp"
	"strconv"
	"strings"
	"time"
)

var gptVersion = regexp.MustCompile(`gpt-(\d+)(?:\.(\d+))?`)

// CacheTTL is how long a provider keeps a used prompt prefix, as its
// docs state it: Anthropic's default ephemeral cache is five minutes;
// OpenAI keeps prefixes for at least 30 minutes on GPT-5.6 and later
// (gpt-6 included) and five to ten on earlier models. Unknown providers
// get the short window, so callers err toward "cold". serve's cache
// chip and the session-title plugin's quiet timer share it.
func CacheTTL(model string) time.Duration {
	m := strings.ToLower(strings.TrimPrefix(model, "~"))
	if gpt := gptVersion.FindStringSubmatch(m); gpt != nil {
		major, _ := strconv.Atoi(gpt[1])
		minor, _ := strconv.Atoi(gpt[2])
		if major > 5 || (major == 5 && minor >= 6) {
			return 30 * time.Minute
		}
	}
	return 5 * time.Minute
}

// AgentTTLer is the optional seam an llm row exposes when the adapter it
// builds for the engine writes cache entries with a TTL of its own
// (llm-anthropic's cache_ttl, 1h by default). The loop's requests keep
// the provider default, which is what CacheTTL answers.
type AgentTTLer interface {
	AgentCacheTTL() time.Duration
}

// AgentCacheTTL is how long an engine session's prompt cache lives on
// the llm service l: the row's own answer when it has one, else what the
// provider keeps by default. The engine writes it into done.usage as
// ttl, so serve's cache chip and the title timer read the right window.
func AgentCacheTTL(l any) time.Duration {
	if t, ok := l.(AgentTTLer); ok {
		return t.AgentCacheTTL()
	}
	if m, ok := l.(LLM); ok {
		return CacheTTL(Name(m))
	}
	return CacheTTL("")
}
