package orb

import (
	"cmp"
	"io"
	"maps"
	"slices"
	"strings"
	"sync"
)

// RedactMinLen is the shortest secret value that is redacted: shorter
// values ("true", a port) would mangle ordinary output.
const RedactMinLen = 8

// Redactor replaces resolved secret values with [redacted:NAME].
// A nil *Redactor passes everything through.
type Redactor struct {
	vals  []string // longest first, so a secret containing another wins
	names []string
}

// NewRedactor builds a redactor over name→value; nil when no value is
// long enough to redact.
func NewRedactor(secrets map[string]string) *Redactor {
	r := &Redactor{}
	type kv struct{ n, v string }
	var all []kv
	for _, n := range slices.Sorted(maps.Keys(secrets)) {
		if v := secrets[n]; len(v) >= RedactMinLen {
			all = append(all, kv{n, v})
		}
	}
	if len(all) == 0 {
		return nil
	}
	slices.SortStableFunc(all, func(a, b kv) int { return cmp.Compare(len(b.v), len(a.v)) })
	for _, e := range all {
		r.vals, r.names = append(r.vals, e.v), append(r.names, e.n)
	}
	return r
}

// scan redacts s. Unless final, it stops at a suffix that is a proper
// prefix of some secret and returns it as held.
func (r *Redactor) scan(s string, final bool) (out, held string) {
	var b strings.Builder
	for i := 0; i < len(s); {
		matched := false
		for k, v := range r.vals {
			// Longest first: a longer secret still arriving is held
			// before a shorter one it contains could match.
			if !final && len(s)-i < len(v) && strings.HasPrefix(v, s[i:]) {
				return b.String(), s[i:]
			}
			if strings.HasPrefix(s[i:], v) {
				b.WriteString("[redacted:" + r.names[k] + "]")
				i += len(v)
				matched = true
				break
			}
		}
		if !matched {
			b.WriteByte(s[i])
			i++
		}
	}
	return b.String(), ""
}

// String redacts a complete text.
func (r *Redactor) String(s string) string {
	if r == nil {
		return s
	}
	out, _ := r.scan(s, true)
	return out
}

// Writer redacts a stream whose chunks may split a secret. Close flushes
// the held tail; it does not close w.
func (r *Redactor) Writer(w io.Writer) io.WriteCloser {
	return &redactWriter{r: r, w: w}
}

type redactWriter struct {
	mu   sync.Mutex
	r    *Redactor
	w    io.Writer
	held string
}

func (rw *redactWriter) Write(p []byte) (int, error) {
	if rw.r == nil {
		return rw.w.Write(p)
	}
	rw.mu.Lock()
	defer rw.mu.Unlock()
	out, held := rw.r.scan(rw.held+string(p), false)
	rw.held = held
	if out != "" {
		if _, err := io.WriteString(rw.w, out); err != nil {
			return 0, err
		}
	}
	return len(p), nil
}

func (rw *redactWriter) Close() error {
	rw.mu.Lock()
	defer rw.mu.Unlock()
	if rw.held == "" {
		return nil
	}
	// The tail can hold a whole shorter secret that waited on a longer one.
	_, err := io.WriteString(rw.w, rw.r.String(rw.held))
	rw.held = ""
	return err
}
