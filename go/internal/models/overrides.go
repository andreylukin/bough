package models

// overrides are facts bough knows before models.dev does: a model that
// launched after the last snapshot, or a rate the catalogue does not
// carry. Each field set here wins over the snapshot and the weekly
// cache; a zero field leaves the catalogue's value alone, so a later
// models.dev entry can still fill what is missing here (a release date).
var overrides = Catalogue{
	"anthropic": {
		// Opus 5.5 launched after the snapshot; without this row an
		// engine session on it shows no cost and no context percentage.
		"claude-opus-5-5": {
			Input:      4,
			Output:     20,
			CacheRead:  0.20,
			CacheWrite: 5.00,
			Context:    1_000_000,
			Efforts:    []string{"low", "medium", "high", "xhigh", "max"},
		},
	},
}

// withOverrides returns c with the overrides applied and, for every
// Anthropic model, the one-hour write rate filled in at 2x input where
// the catalogue has none. c itself is not modified: the maps it holds
// may be shared with a reader.
func withOverrides(c Catalogue) Catalogue {
	out := make(Catalogue, len(c)+len(overrides))
	for p, ms := range c {
		out[p] = ms
	}
	for p, ms := range overrides {
		merged := make(map[string]Model, len(out[p])+len(ms))
		for id, m := range out[p] {
			merged[id] = m
		}
		for id, o := range ms {
			merged[id] = overlay(merged[id], o)
		}
		out[p] = merged
	}
	if ms, ok := out["anthropic"]; ok {
		filled := make(map[string]Model, len(ms))
		for id, m := range ms {
			if m.CacheWrite1h == 0 && m.Input > 0 {
				m.CacheWrite1h = 2 * m.Input
			}
			filled[id] = m
		}
		out["anthropic"] = filled
	}
	return out
}

func overlay(m, o Model) Model {
	if o.Input != 0 {
		m.Input = o.Input
	}
	if o.Output != 0 {
		m.Output = o.Output
	}
	if o.CacheRead != 0 {
		m.CacheRead = o.CacheRead
	}
	if o.CacheWrite != 0 {
		m.CacheWrite = o.CacheWrite
	}
	if o.CacheWrite1h != 0 {
		m.CacheWrite1h = o.CacheWrite1h
	}
	if len(o.Tiers) != 0 {
		m.Tiers = o.Tiers
	}
	if o.Context != 0 {
		m.Context = o.Context
	}
	if o.Release != "" {
		m.Release = o.Release
	}
	if len(o.Efforts) != 0 {
		m.Efforts = o.Efforts
	}
	return m
}
