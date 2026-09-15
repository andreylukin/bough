package wiki

import "regexp"

// Excerpts quote transcripts, and transcripts carry whatever a command
// printed. These are the shapes a credential takes in that output; each
// is replaced with a marker that still says something was there.
var secretREs = []struct {
	re   *regexp.Regexp
	with string
}{
	// JWTs: three base64url segments, the first a JSON header.
	{regexp.MustCompile(`eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}`), "[redacted]"},
	// A password in URL userinfo: scheme://user:secret@host.
	{regexp.MustCompile(`([A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:)[^\s/@]+@`), "${1}[redacted]@"},
	// Authorization: Bearer … / Basic …
	{regexp.MustCompile(`(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}`), "$1 [redacted]"},
	// KEY=value and "key": "value" where the name says it is a secret.
	{regexp.MustCompile(`(?i)\b([A-Za-z0-9_.-]*(?:secret|token|passw(?:or)?d|api[_-]?key|access[_-]?key|private[_-]?key|credential|auth)[A-Za-z0-9_.-]*"?\s*[:=]\s*"?)[^\s"',;]{4,}`), "${1}[redacted]"},
	// Well-known key prefixes, bare.
	{regexp.MustCompile(`\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,})`), "[redacted]"},
}

// redact masks token-like strings in text quoted out of a transcript.
func redact(text string) string {
	for _, s := range secretREs {
		text = s.re.ReplaceAllString(text, s.with)
	}
	return text
}
