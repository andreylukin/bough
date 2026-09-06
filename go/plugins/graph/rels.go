package graph

// The relation vocabulary is closed: Assert refuses a rel that is not
// listed here. A free vocabulary let the same fact land as "requires"
// in one turn and "depends_on" in the next, and neighbors(key, rel)
// could not filter on either. What a model says that fits nothing
// below becomes "relates", with the model's own verb kept in the claim.

import (
	"fmt"
	"slices"
	"strings"
)

// Rel describes one relation: which kinds it joins and whether it is
// something a source stated (observed) or a model inferred.
type Rel struct {
	Src, Dst string // kinds, documentation only; "*" = any
	Inferred bool   // written by a model, never by a collector
	Doc      string
}

// Rels is the vocabulary.
var Rels = map[string]Rel{
	// bough's own record of itself
	"ran":           {"session", "command", false, "the session ran the command"},
	"touches":       {"session", "repo|ticket|pr", false, "the session worked in/on it"},
	"ran_on":        {"session", "model", false, "the session's model"},
	"branched_from": {"session", "session", false, "fork, subagent or compaction parent"},
	"cites":         {"concept", "command|url", false, "a note section cites this evidence"},
	// the external world, from collectors
	"implements": {"pr", "ticket", false, "the PR's branch, title or body names the ticket"},
	"authored":   {"person", "pr|ticket|notion_page|slack_thread", false, "who wrote it"},
	"assigned":   {"person", "ticket", false, "the ticket's assignee"},
	"reviews":    {"person", "pr", false, "a requested or given review"},
	"discusses":  {"slack_thread|notion_page", "pr|ticket|concept", false, "the text links to it"},
	"documents":  {"notion_page", "concept|repo", false, "the page is about it"},
	"mentions":   {"slack_thread|pr", "person", false, "the person is named in it"},
	"awaits":     {"pr|ticket|slack_thread", "person", false, "the person must act next"},
	"has_state":  {"pr|ticket", "state", false, "the current state; closed when it changes"},
	// what a model concluded, author = cheap or session
	"relates":    {"*", "*", true, "connected, the claim says how"},
	"requires":   {"*", "*", true, "needs the other to work"},
	"replaces":   {"*", "*", true, "supersedes the other"},
	"blocked_by": {"*", "*", true, "cannot proceed until the other"},
	"decided":    {"person|session", "decision|concept", true, "a choice that was made"},
}

// RelNames is the vocabulary in a stable order.
func RelNames() []string {
	names := make([]string, 0, len(Rels))
	for n := range Rels {
		names = append(names, n)
	}
	slices.Sort(names)
	return names
}

// NormalizeRel maps a free verb phrase onto the vocabulary. A listed
// rel (any case, spaces or dashes for underscores) is itself; a few
// common synonyms fold; everything else is "relates" and ok = false so
// the caller can keep the original wording in the claim.
func NormalizeRel(rel string) (string, bool) {
	r := strings.ToLower(strings.TrimSpace(rel))
	r = strings.NewReplacer(" ", "_", "-", "_").Replace(r)
	if _, ok := Rels[r]; ok {
		return r, true
	}
	switch r {
	case "depends_on", "needs", "uses", "requires_the":
		return "requires", true
	case "supersedes", "replaced", "obsoletes":
		return "replaces", true
	case "blocked_on", "blocks_on", "waits_for", "waits_on":
		return "blocked_by", true
	case "chose", "decision", "picked", "prefers":
		return "decided", true
	case "wrote", "created", "owns":
		return "authored", true
	case "fixes", "resolves", "closes":
		return "implements", true
	}
	return "relates", false
}

func checkRel(rel string) error {
	if _, ok := Rels[rel]; !ok {
		return fmt.Errorf("graph: %q is not a relation; one of %s", rel, strings.Join(RelNames(), " "))
	}
	return nil
}
