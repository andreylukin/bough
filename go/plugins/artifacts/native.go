package artifacts

import (
	"context"
	"encoding/json"

	"github.com/andreylukin/bough/internal/agenttools"
)

// nativeTools are the four artifact functions as native tools. A page's
// answers still arrive as a job notice that wakes the agent; the tools
// only publish, change and read.
func (s *Store) nativeTools() []agenttools.Tool {
	nameDetail := func(args json.RawMessage) string {
		var a struct{ Name string }
		_ = json.Unmarshal(args, &a)
		return a.Name
	}
	named := func(tool string, fn func(name string) (string, error)) func(context.Context, agenttools.Call) (agenttools.Result, error) {
		return func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
			var a struct {
				Name string `json:"name"`
			}
			if err := agenttools.Decode(tool, c.Args, &a); err != nil {
				return agenttools.Result{}, err
			}
			return result(fn(a.Name)), nil
		}
	}
	nameArg := agenttools.Prop("string", "the page's name")
	return []agenttools.Tool{
		{
			Name:        "artifact",
			Description: "Publish a page for the user, written in OpenUI Lang (read artifact_guide before your first page), and get its URL. Publishing a name again replaces the page; its URL stays.",
			Schema: agenttools.Object([]string{"name", "code"}, map[string]any{
				"name": nameArg,
				"code": agenttools.Prop("string", "the page, in OpenUI Lang"),
			}),
			Detail: nameDetail,
			Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
				var a struct {
					Name string `json:"name"`
					Code string `json:"code"`
				}
				if err := agenttools.Decode("artifact", c.Args, &a); err != nil {
					return agenttools.Result{}, err
				}
				return result(s.Publish(a.Name, a.Code)), nil
			},
		},
		{
			Name:        "artifact_patch",
			Description: "Change a published page statement by statement: a statement with an existing name replaces it, a new one is added, `name = null` removes one. The open page reloads itself.",
			Schema: agenttools.Object([]string{"name", "statements"}, map[string]any{
				"name":       nameArg,
				"statements": agenttools.Prop("string", "`name = Expression` lines"),
			}),
			Detail: nameDetail,
			Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
				var a struct {
					Name       string `json:"name"`
					Statements string `json:"statements"`
				}
				if err := agenttools.Decode("artifact_patch", c.Args, &a); err != nil {
					return agenttools.Result{}, err
				}
				return result(s.Patch(a.Name, a.Statements)), nil
			},
		},
		{
			Name:        "artifact_answers",
			Description: "A published page's form state, the buttons pressed and the notes written, as JSON.",
			Schema:      agenttools.Object([]string{"name"}, map[string]any{"name": nameArg}),
			Detail:      nameDetail,
			Call:        named("artifact_answers", s.Answers),
		},
		{
			Name:        "artifact_guide",
			Description: "The OpenUI Lang reference: syntax and every component's signature. Read it before your first page.",
			Schema:      agenttools.Object(nil, map[string]any{}),
			Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
				return result(s.Guide()), nil
			},
		},
	}
}

func result(out string, err error) agenttools.Result {
	if err != nil {
		return agenttools.Result{Text: out, Error: err.Error()}
	}
	return agenttools.Result{Text: out}
}
