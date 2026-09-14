// Package hookmeta reads hook metadata without evaluating JavaScript.
package hookmeta

import "strings"

// Description returns the first Description: line comment in the header.
// Only whitespace and comments may precede it; block comments are skipped,
// never interpreted as metadata.
func Description(body string) string {
	for {
		body = strings.TrimLeft(body, " \t\r\n\v\f\uFEFF\u00a0\u2028\u2029")
		switch {
		case strings.HasPrefix(body, "//"):
			end := strings.IndexAny(body, "\r\n\u2028\u2029")
			if end < 0 {
				end = len(body)
			}
			line := strings.TrimSpace(body[2:end])
			if value, ok := strings.CutPrefix(line, "Description:"); ok {
				return strings.TrimSpace(value)
			}
			body = body[end:]
		case strings.HasPrefix(body, "/*"):
			end := strings.Index(body[2:], "*/")
			if end < 0 {
				return ""
			}
			body = body[end+4:]
		default:
			return ""
		}
	}
}
