package graph

// Canonical keys and deterministic linking (docs/graph-memory.md, Keys
// and Ingest): a ticket id is the hub key, a PR is repo#number, a repo is
// its normalized origin URL, a person is an email. Linking is string
// extraction, never fuzzy matching.

import (
	"net/url"
	"regexp"
	"strconv"
	"strings"
)

// ticketRe matches Linear-style identifiers: 2+ upper letters, dash,
// digits (NME-1673, FOMS-12). Word-bounded so "SHA-256" style tails in
// prose still match only when they look like a key; callers filter by
// known team prefixes when they have them.
var ticketRe = regexp.MustCompile(`\b([A-Z][A-Z0-9]{1,9}-\d{1,6})\b`)

// prURLRe matches a GitHub pull request URL.
var prURLRe = regexp.MustCompile(`https?://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d+)`)

// prRefRe matches the short form repo#123 (repo = last path segment).
var prRefRe = regexp.MustCompile(`\b([\w.-]+)#(\d+)\b`)

// Tickets extracts ticket identifiers from free text (branch names,
// titles, bodies, Slack), de-duplicated in order of appearance.
func Tickets(text string) []string {
	seen := map[string]bool{}
	var out []string
	for _, m := range ticketRe.FindAllStringSubmatch(text, -1) {
		if !seen[m[1]] {
			seen[m[1]] = true
			out = append(out, m[1])
		}
	}
	return out
}

// PRs extracts pull request keys ("repo#number") from text: full GitHub
// URLs and the short form.
func PRs(text string) []string {
	seen := map[string]bool{}
	var out []string
	add := func(k string) {
		if !seen[k] {
			seen[k] = true
			out = append(out, k)
		}
	}
	for _, m := range prURLRe.FindAllStringSubmatch(text, -1) {
		add(m[2] + "#" + m[3])
	}
	for _, m := range prRefRe.FindAllStringSubmatch(text, -1) {
		add(m[1] + "#" + m[2])
	}
	return out
}

// RepoKey normalizes a git origin (ssh or https, with or without .git)
// to "host/owner/name". A non-URL (a bare path) comes back trimmed,
// which is what command_history.repo held for path-scoped workspaces.
func RepoKey(origin string) string {
	o := strings.TrimSpace(origin)
	if o == "" {
		return ""
	}
	if after, ok := strings.CutPrefix(o, "git@"); ok {
		// git@github.com:owner/name.git
		o = "ssh://" + strings.Replace(after, ":", "/", 1)
	}
	if u, err := url.Parse(o); err == nil && u.Host != "" {
		p := strings.TrimSuffix(strings.Trim(u.Path, "/"), ".git")
		return strings.ToLower(u.Host) + "/" + p
	}
	return strings.TrimSuffix(o, "/")
}

// RepoName is the last segment of a repo key ("bough").
func RepoName(repoKey string) string {
	if _, after, ok := strings.CutLast(repoKey, "/"); ok {
		return after
	}
	return repoKey
}

// Ref is what Resolve understood from a free-form reference.
type Ref struct {
	Kind string
	Key  string
}

// ParseRef classifies a reference the way a person types it: a ticket
// id, a PR URL or repo#n, a git origin or GitHub repo URL, an email, a
// branch name carrying a ticket, or a concept slug. Pure.
func ParseRef(s string) (Ref, bool) {
	s = strings.TrimSpace(s)
	if s == "" {
		return Ref{}, false
	}
	if m := prURLRe.FindStringSubmatch(s); m != nil && strings.HasPrefix(s, "http") {
		return Ref{"pr", m[2] + "#" + m[3]}, true
	}
	if m := prRefRe.FindStringSubmatch(s); m != nil && m[0] == s {
		return Ref{"pr", s}, true
	}
	if m := ticketRe.FindStringSubmatch(s); m != nil && m[0] == s {
		return Ref{"ticket", s}, true
	}
	if strings.Contains(s, "@") && !strings.ContainsAny(s, " /") {
		return Ref{"person", strings.ToLower(s)}, true
	}
	if strings.HasPrefix(s, "git@") || strings.Contains(s, "://") {
		return Ref{"repo", RepoKey(s)}, true
	}
	if t := Tickets(s); len(t) == 1 && strings.Contains(s, "/") {
		return Ref{"ticket", t[0]}, true // a branch name: andrey/NME-1673-fix-thing
	}
	if strings.ContainsAny(s, " \t") {
		return Ref{}, false
	}
	return Ref{"concept", s}, true
}

// PRNumber splits a PR key.
func PRNumber(key string) (repo string, n int, ok bool) {
	before, after, found := strings.CutLast(key, "#")
	if !found {
		return "", 0, false
	}
	n, err := strconv.Atoi(after)
	return before, n, err == nil
}
