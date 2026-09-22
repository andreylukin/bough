package fake

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"slices"
	"strings"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/wrap"
)

// scriptFile is the JSON form llm-script reads.
type scriptFile struct {
	Steps []jsontext.Value `json:"steps"`
}

type scriptStep struct {
	Want   string       `json:"want,omitzero"`
	Text   string       `json:"text,omitzero"`
	Think  string       `json:"think,omitzero"`
	Calls  []scriptCall `json:"calls,omitzero"`
	Stop   string       `json:"stop,omitzero"`
	Usage  *scriptUsage `json:"usage,omitzero"`
	HoldMS int          `json:"hold_ms,omitzero"`
	Error  string       `json:"error,omitzero"`
}

// stepKeys are the keys a step may have. A misspelt key would otherwise
// be ignored, and the step would answer something the author did not
// write.
var stepKeys = []string{"want", "text", "think", "calls", "stop", "usage", "hold_ms", "error"}

type scriptCall struct {
	ID   string         `json:"id,omitzero"`
	Name string         `json:"name"`
	Args jsontext.Value `json:"args,omitzero"`
}

type scriptUsage struct {
	Input      int64 `json:"input"`
	Cached     int64 `json:"cached"`
	CacheWrite int64 `json:"cache_write"`
	Output     int64 `json:"output"`
}

// Overflow is the error text a script step uses for a context overflow:
// it is returned wrapping agentllm.ErrContextOverflow, so the Gate's
// sticky-overflow path can be driven from a script.
const Overflow = "overflow"

// Load reads a script. Every error names the file and the step, since
// it becomes the llm-script row's mount error.
func Load(path string) ([]Step, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("fake: read script: %w", err)
	}
	return Parse(b, path)
}

// Parse is Load on bytes already read; name is used in errors.
func Parse(b []byte, name string) ([]Step, error) {
	var f scriptFile
	if err := json.Unmarshal(b, &f); err != nil {
		return nil, fmt.Errorf("fake: %s: %w", name, err)
	}
	if len(f.Steps) == 0 {
		return nil, fmt.Errorf("fake: %s: no steps", name)
	}
	steps := make([]Step, 0, len(f.Steps))
	for i, raw := range f.Steps {
		step, err := parseStep(raw, i+1)
		if err != nil {
			return nil, fmt.Errorf("fake: %s: step %d: %w", name, i+1, err)
		}
		steps = append(steps, step)
	}
	return steps, nil
}

func parseStep(raw jsontext.Value, n int) (Step, error) {
	var keys map[string]jsontext.Value
	if err := json.Unmarshal(raw, &keys); err != nil {
		return Step{}, err
	}
	var unknown []string
	for k := range keys {
		if !slices.Contains(stepKeys, k) {
			unknown = append(unknown, k)
		}
	}
	if len(unknown) > 0 {
		slices.Sort(unknown)
		return Step{}, fmt.Errorf("unknown keys %v (have %s)", unknown, strings.Join(stepKeys, ", "))
	}
	var s scriptStep
	if err := json.Unmarshal(raw, &s); err != nil {
		return Step{}, err
	}
	return s.step(n)
}

func (s scriptStep) step(n int) (Step, error) {
	out := Step{Want: s.Want, HoldMS: s.HoldMS}
	if s.Error != "" {
		if s.Error == Overflow {
			out.Err = fmt.Errorf("script: %w", agentllm.ErrContextOverflow)
		} else {
			out.Err = errors.New(s.Error)
		}
		return out, nil
	}
	switch s.Stop {
	case "", "complete":
	case "max_output_tokens", "max_tokens":
		out.Stop = ullm.StopMaxOutputTokens
	case "refused", "refusal":
		out.Stop = ullm.StopRefused
	default:
		return Step{}, fmt.Errorf("stop must be complete, max_output_tokens or refused, got %q", s.Stop)
	}
	if s.Think != "" {
		out.Output = append(out.Output, Think(s.Think))
		out.Deltas = append(out.Deltas, agentllm.Delta{Kind: agentllm.DeltaThinking, Text: s.Think})
	}
	if s.Text != "" {
		out.Output = append(out.Output, Text(s.Text))
		for _, w := range Words(s.Text) {
			out.Deltas = append(out.Deltas, agentllm.Delta{Kind: agentllm.DeltaText, Text: w})
		}
	}
	for j, c := range s.Calls {
		if c.Name == "" {
			return Step{}, fmt.Errorf("call %d has no name", j+1)
		}
		id := c.ID
		if id == "" {
			// Derived from the position in the file, so a resumed session
			// replaying the same script gets the same ids.
			id = fmt.Sprintf("call_%d_%d", n, j+1)
		}
		args := "{}"
		if len(c.Args) != 0 {
			if c.Args.Kind() == '"' {
				// A string is taken verbatim: the way to script a model
				// that emits malformed arguments.
				var raw string
				if err := json.Unmarshal(c.Args, &raw); err != nil {
					return Step{}, fmt.Errorf("call %d args: %w", j+1, err)
				}
				args = raw
			} else {
				args = string(bytes.TrimSpace(c.Args))
			}
		}
		out.Output = append(out.Output, Call(id, c.Name, args))
		out.Deltas = append(out.Deltas, agentllm.Delta{Kind: agentllm.DeltaToolStart, CallID: id, Name: c.Name})
	}
	if s.Usage != nil {
		u := s.Usage
		out.Usage = ullm.Usage{
			InputTokens:           u.Input,
			CachedInputTokens:     u.Cached,
			CacheWriteInputTokens: u.CacheWrite,
			OutputTokens:          u.Output,
		}
	}
	if len(out.Output) == 0 && out.Stop == "" {
		return Step{}, fmt.Errorf("needs text, think, calls, stop or error")
	}
	return out, nil
}

// Words splits text the way echo streams it: each piece is a word plus
// the whitespace after it, so the pieces concatenate back to text.
func Words(text string) []string {
	var out []string
	rest := text
	for rest != "" {
		i := strings.IndexAny(rest, " \n")
		if i < 0 {
			out = append(out, rest)
			break
		}
		out = append(out, rest[:i+1])
		rest = rest[i+1:]
	}
	return out
}

// FromStore turns a recorded engine session into a script: one step per
// provider response, in order, with the recorded output. Responses the
// Gate answered without the provider (muted) are skipped, and a
// recorded provider error becomes an error step, so replaying the tape
// makes the same provider calls the session made.
func FromStore(dir, sid string) ([]Step, error) {
	store, err := localfile.New(dir)
	if err != nil {
		return nil, fmt.Errorf("fake: open store %s: %w", dir, err)
	}
	ctx := context.Background()
	var steps []Step
	after := sessionstore.BeforeFirst
	for {
		page, err := store.Items(ctx, session.ID(sid), after, 500)
		if err != nil {
			if errors.Is(err, fs.ErrNotExist) {
				return nil, fmt.Errorf("fake: no session %s in %s", sid, dir)
			}
			return nil, fmt.Errorf("fake: read session %s: %w", sid, err)
		}
		for _, it := range page.Items {
			mr, ok := it.Data.(sessionstore.ModelResponse)
			if !ok {
				continue
			}
			resp := mr.Response
			switch {
			case strings.HasPrefix(resp.ID, "bough-muted-"):
				continue
			case strings.HasPrefix(resp.ID, "bough-error-"):
				steps = append(steps, Step{Err: errors.New("recorded provider error")})
				continue
			}
			steps = append(steps, Step{Output: unsealed(resp.Output), Stop: resp.Stop, Usage: resp.Usage})
		}
		if !page.More {
			break
		}
		after = page.NextAfter
	}
	return steps, nil
}

// unsealed strips the provenance envelopes a recording carries, so the
// replayed items look like what the provider returned.
func unsealed(in []ullm.Item) []ullm.Item {
	out := make([]ullm.Item, len(in))
	for i, it := range in {
		if p, id, ok := strings.Cut(it.ProviderID, "|"); ok && p != "" {
			it.ProviderID = id
		}
		if r, ok := it.Data.(ullm.Reasoning); ok {
			if _, item, ok := wrap.Unwrap(r.Raw); ok {
				r.Raw = item
				it.Data = r
			}
		}
		out[i] = it
	}
	return out
}

// AssertAppendOnly checks the two properties every engine session's
// requests must have: each request's input, except its trailing
// user-side run, is a prefix of the next request's, and every tool call
// has a first result in the user-side run that follows it. A breach of
// the first rewrites the prompt cache and, on Opus 5.5 and Fable 5.1,
// fails the preserved-thinking check; a breach of the second is a 400
// on the Messages API.
func AssertAppendOnly(r Reporter, reqs []Recorded) {
	r.Helper()
	for k := 0; k+1 < len(reqs); k++ {
		cur, next := reqs[k].Request.Input, reqs[k+1].Request.Input
		keep := len(cur) - trailingUserRun(cur)
		if keep > len(next) {
			r.Errorf("fake: request %d has %d committed items, request %d only %d", k+1, keep, k+2, len(next))
			continue
		}
		for i := 0; i < keep; i++ {
			a, b := encode(cur[i]), encode(next[i])
			if a != b {
				r.Errorf("fake: request %d item %d changed in request %d:\n  was %s\n  now %s", k+1, i, k+2, a, b)
				break
			}
		}
	}
	for k, rec := range reqs {
		in := rec.Request.Input
		for i, it := range in {
			c, ok := it.Data.(ullm.ToolCall)
			if !ok {
				continue
			}
			j := i + 1
			for j < len(in) && assistantSide(in[j]) {
				j++
			}
			if j == len(in) {
				continue // the call is the newest output; its results come later
			}
			found := false
			for ; j < len(in) && !assistantSide(in[j]); j++ {
				if res, ok := in[j].Data.(ullm.ToolResult); ok && res.CallID == c.CallID {
					found = true
					break
				}
			}
			if !found {
				r.Errorf("fake: request %d: call %s (%s) has no result in the run after it", k+1, c.CallID, c.Name)
			}
		}
	}
}

func trailingUserRun(in []ullm.Item) int {
	n := 0
	for i := len(in) - 1; i > 0 && !assistantSide(in[i]); i-- {
		n++
	}
	return n
}

func encode(it ullm.Item) string {
	b, err := json.Marshal(it, json.Deterministic(true))
	if err != nil {
		return fmt.Sprintf("%#v", it)
	}
	return string(b)
}
