//go:build !windows

// Package prompt composes the engine's system prompt and the reminders
// that carry later changes to it.
//
// The system text is composed once per session and never edited: a
// changed prefix re-bills the whole prompt cache, and Opus 5.5 / Fable
// 5.1 reject a conversation whose preserved thinking was signed under a
// different system text. What changes afterwards (AGENTS.md, a plugin's
// prompt section, the skills list, MEMORY.md written mid-session) goes
// to the model as a <context-update> block at the head of the next
// input, which is append-only and replays byte for byte.
package prompt

import (
	"crypto/sha256"
	_ "embed"
	"encoding/hex"
	"fmt"
	"runtime"
	"slices"
	"strings"
	"time"
)

//go:embed engine.md
var engineMD string

// Placeholder replaces the harness's own running-call text, which tells
// the model to "end your turn" in words that read as ending the task.
const Placeholder = "This call is still running. Its result arrives later, possibly as a <tool_result call_id=…> block in a user message. Keep working on something independent, or end your reply to wait for it."

// AskSection is the ask row's prompt part on the engine: the loop's
// section names tools.ask, which does not exist as a native call.
const AskSection = "`ask` blocks until the user answers, so ask only what you cannot work out yourself. Pass the choices as `options`, never inlined into the question: options render as clickable rows, inlined ones as prose."

// Part is one named piece of prompt text: a context file (Name is its
// path) or a prompt section (Name is the section's name).
type Part struct{ Name, Text string }

// Skill is one catalogue line: the model reads Path with `view` when it
// wants the skill.
type Skill struct{ Name, Description, Path string }

// Parts is everything the system prompt is composed from.
type Parts struct {
	Preamble     string  // engine.md, or the engine row's system_prompt
	Env          string  // cwd, platform, date (date frozen at build)
	Guidance     string  // task_guidance text when on
	Ask          string  // ask section when ask-answers is mounted
	SessionStart string  // session-start hook context (first build only)
	Context      []Part  // context-md Parts() (AGENTS.md, MEMORY.md, …), in order
	Sections     []Part  // prompt-sections, sorted by name
	Skills       []Skill // catalogue
	Schema       string  // stop-schema SchemaSection
}

// Preamble is engine.md, plus the heartbeat line when the engine row
// sends heartbeats: without them no heartbeat ever arrives, and a line
// promising one would have the model wait for it.
func Preamble(heartbeat time.Duration) string {
	p := strings.TrimSpace(engineMD)
	if heartbeat > 0 {
		p += fmt.Sprintf("\n\nWhile calls are running and nothing else happens for %s, a heartbeat message wakes you with the calls still running. Check they are making progress; end your reply again if they are.", heartbeat)
	}
	return p
}

// Env is the environment part: where the session works, on what, and
// the date it was composed (frozen with the prompt; a later day arrives
// as a reminder).
func Env(cwd string, date time.Time) string {
	s := "Environment:\n- Working directory: " + cwd +
		"\n  Paths you write are relative to it. Never guess an absolute path; list a directory instead.\n- Platform: " + runtime.GOOS
	if runtime.GOOS == "darwin" {
		s += " — BSD userland, not GNU: find has no -printf, sed -i takes an argument, stat uses -f. Prefer portable flags."
	}
	return s + "\n- Date: " + date.Format("2006-01-02")
}

// piece is one reminder-sized unit of the prompt.
type piece struct{ name, text string }

// sortedSections is Sections in name order, whatever order they came in:
// the same sections must compose to the same bytes.
func sortedSections(p Parts) []Part {
	s := slices.Clone(p.Sections)
	slices.SortStableFunc(s, func(a, b Part) int { return strings.Compare(a.Name, b.Name) })
	return s
}

// skillsText renders the catalogue; "" when there are no skills.
func skillsText(skills []Skill) string {
	if len(skills) == 0 {
		return ""
	}
	s := slices.Clone(skills)
	slices.SortStableFunc(s, func(a, b Skill) int { return strings.Compare(a.Name, b.Name) })
	var b strings.Builder
	b.WriteString("Skills: each is a SKILL.md with instructions for one kind of task. When a task matches one, read its SKILL.md with `view` before you start, then follow it.")
	for _, sk := range s {
		b.WriteString("\n- " + sk.Name)
		if d := strings.Join(strings.Fields(sk.Description), " "); d != "" {
			b.WriteString(": " + d)
		}
		b.WriteString(" (" + sk.Path + ")")
	}
	return b.String()
}

// drift is the pieces that can change while a session runs, in prompt
// order. Preamble and Guidance are row config and SessionStart fires
// once, so they are the session's for good and never reminded.
func drift(p Parts) []piece {
	out := []piece{{"environment", p.Env}, {"ask", p.Ask}}
	for _, c := range p.Context {
		out = append(out, piece{c.Name, c.Text})
	}
	for _, s := range sortedSections(p) {
		out = append(out, piece{"prompt-section " + s.Name, s.Text})
	}
	return append(out, piece{"skills", skillsText(p.Skills)}, piece{"stop schema", p.Schema})
}

// Compose is the system prompt, in the §11.1 order. Empty parts leave no
// gap, and the same Parts always give the same bytes.
func Compose(p Parts) string {
	var out []string
	add := func(s string) {
		if s = strings.TrimSpace(s); s != "" {
			out = append(out, s)
		}
	}
	add(p.Preamble)
	add(p.Env)
	add(p.Guidance)
	add(p.Ask)
	add(p.SessionStart)
	for _, c := range p.Context {
		add(c.Text)
	}
	for _, s := range sortedSections(p) {
		add(s.Text)
	}
	add(skillsText(p.Skills))
	add(p.Schema)
	return strings.Join(out, "\n\n")
}

// Hash is the sha256 of Compose, hex: what the engine entry records.
func Hash(p Parts) string {
	sum := sha256.Sum256([]byte(Compose(p)))
	return hex.EncodeToString(sum[:])
}

// Reminder is the <context-update> block that brings a model composed
// from prev up to now, and the names of what changed; "" and nil when
// nothing did. A changed or new piece is sent whole, because the model
// cannot apply a diff to instructions it only half remembers; a removed
// one is named, so its old text stops applying.
func Reminder(prev, now Parts) (text string, changed []string) {
	was := map[string]string{}
	for _, pc := range drift(prev) {
		if t := strings.TrimSpace(pc.text); t != "" {
			was[pc.name] = t
		}
	}
	var body []string
	seen := map[string]bool{}
	for _, pc := range drift(now) {
		t := strings.TrimSpace(pc.text)
		if t == "" {
			continue
		}
		seen[pc.name] = true
		if was[pc.name] == t {
			continue
		}
		changed = append(changed, pc.name)
		body = append(body, t)
	}
	for _, pc := range drift(prev) {
		if strings.TrimSpace(pc.text) == "" || seen[pc.name] {
			continue
		}
		seen[pc.name] = true
		changed = append(changed, pc.name)
		body = append(body, "("+pc.name+" no longer applies: disregard what it said.)")
	}
	if len(changed) == 0 {
		return "", nil
	}
	return "<context-update source=\"" + strings.Join(changed, ", ") + "\">\n" +
		strings.Join(body, "\n\n") + "\n</context-update>", changed
}
