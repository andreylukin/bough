package ask

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
)

// oneAtATime waits until no other native ask or secret is open. The
// engine runs every call in a reply at once, so an ask and a secret can
// be up together, but every UI keeps one answer slot and the last ask
// event wins it: the line typed for the first question answered the
// second, and a credential typed for a secret could land in a plain
// ask, unmasked and recorded. A codemode block asks one question at a
// time by construction, so the loop's path is left as it was.
func (a *Asker) oneAtATime(done <-chan struct{}) (release func(), err error) {
	if a.expireDir != "" {
		// Test use only (see expireDir): a "hold" file there keeps the
		// question from being put in until it goes, so a model test can
		// stand in the state where the call has started and nothing is
		// asked yet, which in real time lasts a microsecond.
		if !waitGone(filepath.Join(a.expireDir, "hold"), done) {
			return nil, fmt.Errorf("ask: cancelled with no answer")
		}
	}
	a.mu.Lock()
	if a.native == nil {
		a.native = make(chan struct{}, 1)
	}
	turn := a.native
	a.mu.Unlock()
	select {
	case turn <- struct{}{}:
		return func() { <-turn }, nil
	case <-done:
		return nil, fmt.Errorf("ask: cancelled with no answer")
	}
}

// waitGone polls until file does not exist (true) or done closes (false).
func waitGone(file string, done <-chan struct{}) bool {
	tick := time.NewTicker(10 * time.Millisecond)
	defer tick.Stop()
	for {
		if _, err := os.Stat(file); os.IsNotExist(err) {
			return true
		}
		select {
		case <-done:
			return false
		case <-tick.C:
		}
	}
}

// nativeTools are ask and secret for an engine that calls tools
// natively. Both are Blocking: the wait is on the person, so the call
// holds its turn with no settle and no call timeout, as tools.ask
// holds its block. The history entries are the codemode path's.
func (a *Asker) nativeTools() []agenttools.Tool {
	return []agenttools.Tool{
		{
			Name:        "ask",
			Description: "Ask the USER a question and wait for the answer. Give options when the answer is one of a few choices; they render as buttons.",
			Schema: agenttools.Object([]string{"question"}, map[string]any{
				"question": agenttools.Prop("string", "what to ask"),
				"options":  map[string]any{"type": "array", "items": map[string]any{"type": "string"}, "description": "choices to offer"},
			}),
			Blocking: true,
			Detail: func(args json.RawMessage) string {
				var v struct{ Question string }
				_ = json.Unmarshal(args, &v)
				return v.Question
			},
			Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
				var v struct {
					Question string   `json:"question"`
					Options  []string `json:"options"`
				}
				if err := agenttools.Decode("ask", c.Args, &v); err != nil {
					return agenttools.Result{}, err
				}
				release, err := a.oneAtATime(ctx.Done())
				if err != nil {
					return agenttools.Result{Error: err.Error()}, nil
				}
				defer release()
				out, err := a.putIn(ctx.Done(), nil, v.Question, false, v.Options...)
				if err != nil {
					return agenttools.Result{Error: err.Error()}, nil
				}
				return agenttools.Result{Text: out}, nil
			},
		},
		{
			Name:        "secret",
			Description: "Ask the user for a credential, store it in the keychain and add it to the project's secrets. You never see the value; commands get it as an environment variable, so never print it.",
			Schema: agenttools.Object([]string{"name", "question"}, map[string]any{
				"name":     agenttools.Prop("string", "the environment variable name, e.g. STRIPE_KEY"),
				"question": agenttools.Prop("string", "why it is needed, shown to the user"),
				"project":  agenttools.Prop("string", "project slug; defaults to this session's project"),
			}),
			Blocking: true,
			Detail: func(args json.RawMessage) string {
				var v struct{ Name string }
				_ = json.Unmarshal(args, &v)
				return v.Name
			},
			Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
				var v struct {
					Name     string `json:"name"`
					Question string `json:"question"`
					Project  string `json:"project"`
				}
				if err := agenttools.Decode("secret", c.Args, &v); err != nil {
					return agenttools.Result{}, err
				}
				var project []string
				if v.Project != "" {
					project = []string{v.Project}
				}
				release, err := a.oneAtATime(ctx.Done())
				if err != nil {
					return agenttools.Result{Error: "secret: " + err.Error()}, nil
				}
				defer release()
				// The value goes from the answer straight to the keychain
				// inside secretVia; only "stored NAME as REF" comes back,
				// so it never reaches the op state, the store or the model.
				out, err := a.secretVia(func(q string) (string, error) {
					return a.putIn(ctx.Done(), nil, q, true)
				}, v.Name, v.Question, project...)
				if err != nil {
					return agenttools.Result{Error: err.Error()}, nil
				}
				return agenttools.Result{Text: out}, nil
			},
		},
	}
}
