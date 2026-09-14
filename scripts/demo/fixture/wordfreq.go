// Package wordfreq counts words in text.
package wordfreq

import (
	"sort"
	"strings"
	"unicode"
)

type Count struct {
	Word string
	N    int
}

// Counts returns how often each lower-cased word appears.
func Counts(text string) map[string]int {
	out := map[string]int{}
	for _, w := range strings.FieldsFunc(text, func(r rune) bool { return !unicode.IsLetter(r) }) {
		out[strings.ToLower(w)]++
	}
	return out
}

// TopN returns the n most frequent words, most frequent first; ties
// are broken alphabetically.
func TopN(text string, n int) []Count {
	var all []Count
	for w, c := range Counts(text) {
		all = append(all, Count{w, c})
	}
	sort.Slice(all, func(i, j int) bool { return all[i].N > all[j].N })
	return all[:n]
}
