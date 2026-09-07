package ui

// The cache chip: whether the provider's prompt cache is still hot.
// Anthropic keeps a cached prefix for five minutes after it was last
// used, so a turn started inside that window re-reads the conversation
// at a tenth of the price, and one started after it pays full input
// again — the difference between a $0.05 turn and a $0.50 one on a big
// context. The bar says which it will be: "⚡ cache hot" while the
// window is open, "❄ cache cold" once it has passed. A resumed session
// counts from its last turn on file, so coming straight back reads
// hot, and coming back tomorrow reads cold before the first prompt is
// paid for. Shown only once the provider has reported cache tokens at
// all; a provider without a cache gets no chip.

import (
	"time"

	tea "charm.land/bubbletea/v2"
)

// cacheTTL is how long a used prefix stays cached (Anthropic's default
// ephemeral cache; OpenAI's automatic caching is about the same).
const cacheTTL = 5 * time.Minute

// cacheTickMsg redraws the bar when the window closes.
type cacheTickMsg struct{}

// cacheChip is the bar's cache segment, "" when there is nothing to
// say: no request yet, or a provider that reports no cache tokens.
func (m *model) cacheChip(cfg *uiCfg) string {
	if m.lastRequest.IsZero() || cfg.usage == nil {
		return ""
	}
	u := cfg.usage.Usage()
	if u.CacheReadTokens+u.CacheCreationTokens == 0 {
		return ""
	}
	if time.Since(m.lastRequest) < cacheTTL {
		return cfg.theme["accent"].Render("⚡ cache hot")
	}
	return cfg.theme["error"].Render("❄ cache cold")
}

// cacheTick wakes the bar just after the window closes, so the chip
// turns cold on its own.
func (m *model) cacheTick() tea.Cmd {
	if m.lastRequest.IsZero() {
		return nil
	}
	left := cacheTTL - time.Since(m.lastRequest)
	if left < 0 {
		return nil
	}
	return tea.Tick(left+time.Second, func(time.Time) tea.Msg { return cacheTickMsg{} })
}
