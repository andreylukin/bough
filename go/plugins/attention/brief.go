package attention

// Briefs: one plain sentence per piece of work, for a reader who is
// not the person doing it. Written by llm-small from the facts the
// board already has, cached by those facts, so a row costs one call
// until something about it changes.

import (
	"context"
	"crypto/sha1"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

const briefPrompt = `You write one-sentence status lines about a software engineer's work, for their manager to read.
Rules: one sentence, at most 28 words, plain English, third person (the engineer is named in the facts), no ids, no keys, no markdown.
Say "checks" rather than "CI", "review" rather than "PR review", "automated agent" for pr-watch or a session.
Say what the work is for, where it stands, and what or who it waits on. Present tense. Reply with the sentence only.`

const headlinePrompt = `You summarise a software engineer's current work for their manager in one sentence of at most 30 words.
Plain English, third person (the engineer is named in the facts), no ids, no markdown, no lists.
Say "checks" rather than "CI". Lead with what needs the engineer, then what is moving, then what is stuck.
Reply with the sentence only.`

// briefs is the cache: key -> entry, persisted beside the graph.
type briefs struct {
	mu      sync.Mutex
	path    string
	entries map[string]briefEntry
	pending map[string]bool
	small   llm.LLM
}

type briefEntry struct {
	Hash string    `json:"hash"`
	Text string    `json:"text"`
	At   time.Time `json:"at"`
}

func newBriefs(path string, small llm.LLM) *briefs {
	b := &briefs{path: path, entries: map[string]briefEntry{}, pending: map[string]bool{}, small: small}
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &b.entries)
	}
	return b
}

func (b *briefs) save() {
	data, err := json.MarshalIndent(b.entries, "", " ")
	if err != nil {
		return
	}
	_ = os.MkdirAll(filepath.Dir(b.path), 0o755)
	_ = os.WriteFile(b.path, data, 0o644)
}

func hashOf(parts ...string) string {
	h := sha1.Sum([]byte(strings.Join(parts, "\x1f")))
	return hex.EncodeToString(h[:8])
}

// get returns the brief for key when its facts hash matches; else it
// starts one (once) and reports pending.
func (b *briefs) get(key, hash, prompt, input string) (text string, pending bool) {
	b.mu.Lock()
	if e, ok := b.entries[key]; ok && e.Hash == hash {
		b.mu.Unlock()
		return e.Text, false
	}
	if b.small == nil {
		b.mu.Unlock()
		return "", false
	}
	if b.pending[key] {
		b.mu.Unlock()
		return "", true
	}
	b.pending[key] = true
	b.mu.Unlock()
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		reply, err := b.small.Complete(ctx, prompt, []llm.Message{{Role: "user", Content: input}})
		b.mu.Lock()
		defer b.mu.Unlock()
		delete(b.pending, key)
		if err != nil {
			return
		}
		b.entries[key] = briefEntry{Hash: hash, Text: cleanBrief(reply), At: time.Now()}
		b.save()
	}()
	return "", true
}

// cleanBrief trims what a small model wraps around a sentence.
func cleanBrief(s string) string {
	if answer, ok := loop.StopAnswer(s); ok {
		s = answer
	}
	s = strings.TrimSpace(s)
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		s = strings.TrimSpace(s[:i])
	}
	return strings.Trim(s, ` "'*`)
}

// Brief is the sentence for an item, and whether one is being written.
func (s *Service) Brief(kind, key string) (string, bool) {
	if s.briefs == nil {
		return "", false
	}
	b := s.Board()
	var it Item
	found := false
	for _, col := range [][]Item{b.Me, b.Motion, b.Others} {
		for _, x := range col {
			if x.Key == key {
				it, found = x, true
			}
		}
	}
	if !found {
		return "", false
	}
	if it.Count > 0 {
		t := fmt.Sprintf("%d automated dependency updates are waiting for review", it.Count)
		if n := strings.TrimSpace(strings.TrimPrefix(it.Status, "ci failing")); strings.HasPrefix(it.Status, "ci failing") {
			if n == "" || n == fmt.Sprintf("×%d", it.Count) {
				t += "; checks are failing on all of them"
			} else {
				t += "; checks are failing on " + strings.TrimPrefix(n, "×") + " of them"
			}
		}
		return t + ".", false
	}
	var facts []string
	facts = append(facts, "engineer: "+s.myName(), "kind: "+it.Kind, "title: "+it.Title, "status: "+it.Status, "column: "+columnOf(b, key), "asks: "+it.Detail, "age: "+shortAgeText(s.Now().Sub(it.Since)))
	if it.Summary != "" {
		facts = append(facts, "source: "+it.Summary)
	}
	for _, l := range s.Detail(kind, key) {
		if l.Label == "sessions" {
			continue
		}
		facts = append(facts, l.Label+": "+l.Text)
	}
	input := strings.Join(facts, "\n")
	return s.briefs.get(kind+":"+key, hashOf(input), briefPrompt, input)
}

// Headline is one sentence for the whole board.
func (s *Service) Headline() (string, bool) {
	if s.briefs == nil {
		return "", false
	}
	b := s.Board()
	var lines []string
	add := func(label string, items []Item) {
		for _, it := range items {
			t := it.Title
			if it.Count > 0 {
				t = fmt.Sprintf("%d dependency updates", it.Count)
			}
			lines = append(lines, fmt.Sprintf("%s: %s (%s; %s; %s)", label, t, it.Status, it.Detail, shortAgeText(s.Now().Sub(it.Since))))
		}
	}
	add("needs the engineer", b.Me)
	add("an automated agent is working on it", b.Motion)
	add("waiting on other people", b.Others)
	input := "engineer: " + s.myName() + "\n" + strings.Join(lines, "\n")
	return s.briefs.get("headline", hashOf(input), headlinePrompt, input)
}

// myName is the person's name: git's user.name, else the graph's, else
// "the engineer".
func (s *Service) myName() string {
	if out, err := exec.Command("git", "config", "--get", "user.name").Output(); err == nil {
		if n := strings.TrimSpace(string(out)); n != "" {
			return strings.Fields(n)[0] // first name reads better in a sentence
		}
	}
	if s.graph != nil && s.graph.Me != "" {
		if e, err := s.graph.Store.Get("person", s.graph.Me); err == nil && e.Title != "" && !strings.Contains(e.Title, "@") {
			return e.Title
		}
	}
	return "the engineer"
}

func columnOf(b Board, key string) string {
	for _, it := range b.Me {
		if it.Key == key {
			return "needs me"
		}
	}
	for _, it := range b.Motion {
		if it.Key == key {
			return "an agent is working on it"
		}
	}
	return "waiting on others"
}

func shortAgeText(d time.Duration) string {
	switch {
	case d < time.Hour:
		return "under an hour"
	case d < 48*time.Hour:
		return fmt.Sprintf("%d hours", int(d.Hours()))
	case d < 21*24*time.Hour:
		return fmt.Sprintf("%d days", int(d.Hours()/24))
	default:
		return fmt.Sprintf("%d weeks", int(d.Hours()/24/7))
	}
}
