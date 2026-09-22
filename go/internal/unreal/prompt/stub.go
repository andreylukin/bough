// Package prompt is W5's (go/docs/unreal-engine.md §5.3, §11.1). This
// file is the B0 stub W2 wrote because B0 never ran: the frozen
// signatures, plus W5's additive Preamble, Env and AskSection, with
// bodies just good enough for the session tests to run a coordinator.
// W5's real package replaces it; delete this file at integration.
package prompt

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"runtime"
	"sort"
	"strings"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

const Placeholder = "This call is still running. Its result arrives later, possibly as a <tool_result call_id=…> block in a user message. Keep working on something independent, or end your reply to wait for it."

const AskSection = "`ask` blocks until the user answers, so ask only what you cannot work out yourself. Pass the choices as `options`, never inlined into the question."

type Part struct{ Name, Text string }
type Skill struct{ Name, Description, Path string }

type Parts struct {
	Preamble     string
	Env          string
	Guidance     string
	Ask          string
	SessionStart string
	Context      []Part
	Sections     []Part
	Skills       []Skill
	Schema       string
}

func Preamble(heartbeat time.Duration) string {
	return "You are bough, a coding agent. Tool calls run asynchronously; end your reply to wait for a call still running."
}

func Env(cwd string, date time.Time) string {
	return fmt.Sprintf("Working directory: %s\nPlatform: %s\nDate: %s", cwd, runtime.GOOS, date.Format("2006-01-02"))
}

func pieces(p Parts) []Part {
	out := []Part{{"preamble", p.Preamble}, {"env", p.Env}, {"guidance", p.Guidance}, {"ask", p.Ask}, {"session-start", p.SessionStart}}
	out = append(out, p.Context...)
	secs := append([]Part(nil), p.Sections...)
	sort.Slice(secs, func(i, j int) bool { return secs[i].Name < secs[j].Name })
	out = append(out, secs...)
	var sk []string
	for _, s := range p.Skills {
		sk = append(sk, "- "+s.Name+": "+s.Description+" ("+s.Path+")")
	}
	out = append(out, Part{"skills", strings.Join(sk, "\n")}, Part{"schema", p.Schema})
	return out
}

func Compose(p Parts) string {
	var b []string
	for _, x := range pieces(p) {
		if strings.TrimSpace(x.Text) != "" {
			b = append(b, strings.TrimSpace(x.Text))
		}
	}
	return strings.Join(b, "\n\n")
}

func Hash(p Parts) string {
	s := sha256.Sum256([]byte(Compose(p)))
	return hex.EncodeToString(s[:])
}

// Reminder diffs the drifting pieces (everything but the preamble, env
// and session-start, which are fixed at the first build).
func Reminder(prev, now Parts) (text string, changed []string) {
	was := map[string]string{}
	for _, x := range pieces(prev) {
		was[x.Name] = x.Text
	}
	var body []string
	for _, x := range pieces(now) {
		switch x.Name {
		case "preamble", "env", "session-start":
			continue
		}
		if was[x.Name] != x.Text {
			changed = append(changed, x.Name)
			body = append(body, x.Text)
		}
	}
	if len(changed) == 0 {
		return "", nil
	}
	return fmt.Sprintf("<context-update source=%q>\n%s\n</context-update>", strings.Join(changed, ", "), strings.Join(body, "\n\n")), changed
}

func Wrap(inner contextbuilder.Builder, system, placeholder string) contextbuilder.Builder {
	return &wrapped{Builder: inner, system: system, placeholder: placeholder}
}

type wrapped struct {
	contextbuilder.Builder
	system, placeholder string
}

func (w *wrapped) Build() (contextbuilder.Result, error) {
	res, err := w.Builder.Build()
	if err != nil {
		return res, err
	}
	in := append([]ullm.Item(nil), res.Request.Input...)
	if len(in) > 0 {
		in[0] = ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: w.system}}
	}
	for i, it := range in {
		tr, ok := it.Data.(ullm.ToolResult)
		if !ok || len(tr.Output) != 1 || tr.Output[0].Value != contextbuilder.ToolCallRunningPayload {
			continue
		}
		in[i] = ullm.Item{ProviderID: it.ProviderID, Type: it.Type, Data: ullm.ToolResult{CallID: tr.CallID, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: w.placeholder}}}}
	}
	res.Request.Input = in
	return res, nil
}
