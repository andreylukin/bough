package title

import (
	"fmt"
	"regexp"
	"strings"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/loop"
)

// TurnPrompt writes one line of the running log for one finished turn.
const TurnPrompt = `Keep running log of coding-agent session. Given log so far + ONE new turn.
Write ONE line, max 16 words, caveman style: drop articles (a/an/the), filler, pleasantries, hedging. Fragments fine.
Shape: "You <asked what>; agent <concrete result>." Result names substance: what found, built, changed, failed, or what agent waits on. Never bare "done".
Say "You" for person, "agent" for assistant, never "user". Keep file names, commands, errors, numbers exact. No preamble.`

// FinalPrompt names the session from its running log. Two labelled
// lines rather than JSON: small models break JSON far more often than
// they break a "Title:" prefix.
const FinalPrompt = `Name coding session from running log. Caveman style: drop articles, filler, hedging. Fragments fine. Technical terms exact.
Exactly two lines:
Title: 3-6 words, what session is about overall. No quotes, no period, no "session"/"task".
Summary: max 25 words. What you work on. What done. Where stands now. Say "You" for person, "agent" for assistant, never "user".
Nothing else.`

// maxWords caps a log line whatever the model wrote.
const maxWords = 16

// Turn is one request and what came of it, read back from history.
type Turn struct {
	Ask   string
	Calls []string // each tool call's gist and exit
	Reply string   // the last non-empty assistant text
	End   string   // "done", "cancelled", "error"; "" while still open
}

var toolCall = regexp.MustCompile("tools\\.(\\w+)\\(\\s*[\"`']?([^\"`'\\n]{0,140})")

// cut keeps the first n runes of a trimmed string.
func cut(s string, n int) string {
	s = strings.TrimSpace(s)
	if r := []rune(s); len(r) > n {
		return string(r[:n]) + "…"
	}
	return s
}

// gist names a code block by its first tool call, else its first line.
func gist(code string) string {
	if m := toolCall.FindStringSubmatch(code); m != nil {
		return m[1] + ": " + m[2]
	}
	first, _, _ := strings.Cut(code, "\n")
	return cut(first, 140)
}

// Turns groups a session's entries into turns: an input opens one, a
// done closes it. A cancelled or error entry before the done names how it
// ended (the loop writes non-terminal errors mid-turn too, so they never
// close one). An input arriving while a turn is open closes that one as
// still open. A turn's number is its index here plus one, everywhere.
func Turns(entries []history.Entry) []Turn {
	var out []Turn
	var cur *Turn
	pending, end := "", ""
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		if e.Kind == "input" {
			if cur != nil && cur.Ask != "" {
				out = append(out, *cur)
			}
			cur, pending, end = &Turn{Ask: history.Prompt(e)}, "", ""
			continue
		}
		if cur == nil {
			continue
		}
		switch e.Kind {
		case "code":
			pending = gist(text)
		case "result":
			call := pending
			if call == "" {
				call = "call"
			}
			if exit, ok := e.Data["exit"]; ok && exit != nil {
				call += fmt.Sprintf(" → exit %v", exit)
			}
			cur.Calls = append(cur.Calls, call)
			pending = ""
		case "assistant":
			if strings.TrimSpace(text) != "" {
				cur.Reply = text
			}
		case "cancelled", "error":
			end = e.Kind
		case "done":
			cur.End = "done"
			if end != "" {
				cur.End = end
			}
			out = append(out, *cur)
			cur = nil
		}
	}
	if cur != nil && cur.Ask != "" {
		out = append(out, *cur)
	}
	return out
}

// render is the new turn as the log writer reads it.
func render(t Turn) string {
	calls := t.Calls
	if len(calls) > 12 {
		calls = append(append(append([]string{}, calls[:4]...), fmt.Sprintf("… %d more …", len(calls)-8)), calls[len(calls)-4:]...)
	}
	var b strings.Builder
	b.WriteString("User asked: " + cut(t.Ask, 800))
	if len(calls) > 0 {
		b.WriteString("\nAgent ran:")
		for _, c := range calls {
			b.WriteString("\n- " + c)
		}
	}
	end := t.End
	if end == "" {
		end = "still open"
	}
	b.WriteString("\nAgent's last reply: " + cut(t.Reply, 1200) + "\nTurn ended: " + end)
	return b.String()
}

// TurnInput is the user message for one turn's log line: the last dozen
// lines of the log so far, then the turn.
func TurnInput(log []string, t Turn) string {
	if len(log) > 12 {
		log = log[len(log)-12:]
	}
	so := strings.Join(log, "\n")
	if so == "" {
		so = "(empty)"
	}
	return "Log so far:\n" + so + "\n\nNew turn:\n" + render(t)
}

var numbered = regexp.MustCompile(`^\d+\.\s*`)

// CleanLine makes a model's reply one log line: the first non-empty
// line, no numbering it copied from the log, no escapes, control or
// bidi runes, at most maxWords words.
func CleanLine(s string) string {
	if answer, ok := loop.StopAnswer(s); ok {
		s = answer
	}
	for _, l := range strings.Split(s, "\n") {
		if l = oneLine(l); l != "" {
			s = l
			break
		}
		s = ""
	}
	s = strings.Trim(numbered.ReplaceAllString(s, ""), ` "'*`)
	if w := words(s); len(w) > maxWords {
		s = strings.TrimRight(strings.Join(w[:maxWords], " "), ",;:") + "…"
	}
	return s
}

// words splits on spaces, keeping a backticked span (a command with
// spaces in it) as one word, so the cap does not eat the result.
func words(s string) []string {
	var out []string
	var b strings.Builder
	tick := false
	for _, r := range s {
		switch {
		case r == '`':
			tick = !tick
			b.WriteRune(r)
		case r == ' ' && !tick:
			if b.Len() > 0 {
				out = append(out, b.String())
				b.Reset()
			}
		default:
			b.WriteRune(r)
		}
	}
	if b.Len() > 0 {
		out = append(out, b.String())
	}
	return out
}

// pending is the turns to log after the logged ones: every turn past
// logged that has ended, or was cut off by a later input (the trailing
// open turn waits for its done). More than two behind (a session that
// predates the log) logs just the latest.
func pending(ts []Turn, logged int) []int {
	var ns []int
	for i := logged; i < len(ts); i++ {
		if ts[i].End != "" || i < len(ts)-1 {
			ns = append(ns, i+1)
		}
	}
	if len(ns) > 2 {
		ns = ns[len(ns)-1:]
	}
	return ns
}

// provisional names a session from its first log line until the final
// naming: "You fixed x; agent ..." reads as "Fixed x".
func provisional(line string) string {
	ask, _, _ := strings.Cut(line, ";")
	ask = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(ask), "You "))
	if r := []rune(ask); len(r) > 0 {
		ask = strings.ToUpper(string(r[0])) + string(r[1:])
	}
	return Clean(ask)
}

// turnLog is the session's running log as the prompts show it ("3. You
// ...") and the highest turn it covers.
func turnLog(entries []history.Entry) (lines []string, last int) {
	for _, e := range entries {
		if e.Kind != "turn-summary" {
			continue
		}
		text, _ := e.Data["text"].(string)
		n := intOf(e.Data["turn"])
		lines = append(lines, fmt.Sprintf("%d. %s", n, text))
		last = max(last, n)
	}
	return lines, last
}

// lastFinal is the turn the last final naming covered, 0 for none.
func lastFinal(entries []history.Entry) int {
	n := 0
	for _, e := range entries {
		if e.Kind == "title" && e.Data["final"] == true {
			n = intOf(e.Data["turn"])
		}
	}
	return n
}

// intOf reads a number that is an int in memory and a float64 once it
// has round-tripped through JSON.
func intOf(v any) int {
	switch n := v.(type) {
	case int:
		return n
	case float64:
		return int(n)
	}
	return 0
}
