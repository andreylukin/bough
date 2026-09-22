package wiki

// Triage: what the person said about the brief's rows. A dismissed row
// stays gone, a pinned one stays first, and a rule is a sentence in
// profile.md the agent reads before it writes the next brief. Nothing
// here is inferred: every inclusion and exclusion traces to a line the
// person can read and delete.

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// Triage is topics/me/triage.json.
type Triage struct {
	// Dismissed is signal key -> the day it was dismissed.
	Dismissed map[string]string `json:"dismissed"`
	// Pinned is the keys to list first until they are done.
	Pinned []string `json:"pinned"`
}

var ErrNoProfile = errors.New("wiki: no topics/me/profile.md")

func (p paths) triage() string { return filepath.Join(p.me(), "triage.json") }

// Triage reads the file; an absent one is empty, not an error.
func (s *Store) Triage() Triage {
	t := Triage{Dismissed: map[string]string{}, Pinned: []string{}}
	b, err := os.ReadFile(s.p.triage())
	if err != nil {
		return t
	}
	_ = json.Unmarshal(b, &t)
	if t.Dismissed == nil {
		t.Dismissed = map[string]string{}
	}
	if t.Pinned == nil {
		t.Pinned = []string{}
	}
	return t
}

// Mark applies one action to one key: dismiss, undismiss, pin, unpin.
func (s *Store) Mark(action, key string, now time.Time) (Triage, error) {
	key = strings.TrimSpace(key)
	if key == "" {
		return Triage{}, fmt.Errorf("wiki: triage: empty key")
	}
	t := s.Triage()
	switch action {
	case "dismiss":
		t.Dismissed[key] = now.Format("2006-01-02")
		t.Pinned = without(t.Pinned, key)
	case "undismiss":
		delete(t.Dismissed, key)
	case "pin":
		if !contains(t.Pinned, key) {
			t.Pinned = append(t.Pinned, key)
		}
		delete(t.Dismissed, key)
	case "unpin":
		t.Pinned = without(t.Pinned, key)
	default:
		return Triage{}, fmt.Errorf("wiki: triage: unknown action %q", action)
	}
	if err := os.MkdirAll(s.p.me(), 0o755); err != nil {
		return Triage{}, err
	}
	b, _ := json.MarshalIndent(t, "", "  ")
	return t, os.WriteFile(s.p.triage(), append(b, '\n'), 0o644)
}

// Profile sections the agent reads for relevance. Steer files a sentence
// under one of them; Rule files under Not mine.
const (
	SectionMine    = "Mine"
	SectionNotMine = "Not mine"
	SectionWatch   = "Watch"
)

// Steer appends a sentence to profile.md under Watch or Not mine: a
// sentence that starts with ignore, skip, hide, drop, not or no is an
// exclusion, anything else is something to watch. The section is
// created at the end of the profile when it is missing.
func (s *Store) Steer(text string, now time.Time) (section string, err error) {
	text = strings.TrimSpace(strings.Join(strings.Fields(text), " "))
	if text == "" {
		return "", fmt.Errorf("wiki: steer: nothing to add")
	}
	section = SectionWatch
	first := strings.ToLower(strings.TrimLeft(text, "-•* "))
	for _, w := range []string{"ignore", "skip", "hide", "drop", "not ", "no ", "never", "stop showing", "don't", "dont"} {
		if strings.HasPrefix(first, w) {
			section = SectionNotMine
			break
		}
	}
	return section, s.appendRule(section, text, now)
}

// Rule appends a sentence under Not mine (a dismissal that taught something).
func (s *Store) Rule(text string, now time.Time) error {
	text = strings.TrimSpace(text)
	if text == "" {
		return fmt.Errorf("wiki: rule: nothing to add")
	}
	return s.appendRule(SectionNotMine, text, now)
}

func (s *Store) appendRule(section, text string, now time.Time) error {
	b, err := os.ReadFile(s.p.profile())
	if err != nil {
		return ErrNoProfile
	}
	line := "- " + text + " (added " + now.Format("2006-01-02") + ")"
	lines := strings.Split(strings.TrimRight(string(b), "\n"), "\n")
	head := "## " + section
	at := -1
	for i, l := range lines {
		if strings.TrimSpace(l) == head {
			at = i
		}
	}
	if at < 0 {
		lines = append(lines, "", head, "", line)
	} else {
		// After the section's last non-empty line, before the next heading.
		end := len(lines)
		for i := at + 1; i < len(lines); i++ {
			if strings.HasPrefix(lines[i], "## ") {
				end = i
				break
			}
		}
		last := end
		for last > at+1 && strings.TrimSpace(lines[last-1]) == "" {
			last--
		}
		lines = append(lines[:last], append([]string{line}, lines[last:]...)...)
	}
	return os.WriteFile(s.p.profile(), []byte(strings.Join(lines, "\n")+"\n"), 0o644)
}

func contains(xs []string, x string) bool {
	for _, y := range xs {
		if y == x {
			return true
		}
	}
	return false
}

func without(xs []string, x string) []string {
	out := xs[:0:0]
	for _, y := range xs {
		if y != x {
			out = append(out, y)
		}
	}
	sort.Strings(out)
	return out
}
