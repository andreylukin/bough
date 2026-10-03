//go:build !windows

package session

import (
	"slices"

	"github.com/unreallabsai/unreal-agent/harness/contextbuilder"
	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
)

type taskInput struct {
	text  string
	start bool
}

// The coordinator may admit another input after Build and before Respond.
// Freeze task provenance into that request, not shared mutable Gate state.
// This private Go value is consumed before projection: neither provider text,
// editable notes nor tool results can manufacture an authenticated task.
type taskEnvelope struct{ items []ullm.Item }

type taskBuilder struct {
	contextbuilder.Builder
	resolve func(string) (taskInput, bool)
	active  []ullm.Item
}

func (b *taskBuilder) AddExternalInput(in inbox.Input) error {
	if err := b.Builder.AddExternalInput(in); err != nil {
		return err
	}
	if task, ok := b.resolve(string(in.ID)); ok {
		if task.start {
			b.active = nil
		}
		b.active = append(b.active, ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: task.text}})
	}
	return nil
}

func (b *taskBuilder) Build() (contextbuilder.Result, error) {
	r, err := b.Builder.Build()
	if err == nil {
		r.Request.Input = append(slices.Clone(r.Request.Input), ullm.Item{Data: taskEnvelope{items: slices.Clone(b.active)}})
	}
	return r, err
}

func extractTask(req ullm.Request) (ullm.Request, []ullm.Item) {
	var task []ullm.Item
	in := make([]ullm.Item, 0, len(req.Input))
	for _, it := range req.Input {
		if envelope, ok := it.Data.(taskEnvelope); ok {
			task = envelope.items
		} else {
			in = append(in, it)
		}
	}
	req.Input = in
	return req, task
}

func (r *Runtime) taskInput(id string) (taskInput, bool) {
	// Only actor-admitted user inputs have this audit provenance. Wakes have
	// an explicit flag; notices and lifecycle nudges have no matching input.
	// Replay feeds these same IDs on resume/fork, without trusting note text.
	for _, e := range r.d.History.Entries() {
		if e.Kind != "input" || e.Data["input_id"] != id || e.Data["wake"] == true {
			continue
		}
		text, ok := e.Data["text"].(string)
		if !ok {
			return taskInput{}, false
		}
		if r.d.Redact != nil {
			text = r.d.Redact(text)
		}
		return taskInput{text: text, start: e.Data["steer"] != true}, true
	}
	return taskInput{}, false
}
