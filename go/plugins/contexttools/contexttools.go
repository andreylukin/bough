// Package contexttools exposes the calling CLM session's editable context.
// Registry entries carry no session state; the engine binds each call's capability.
package contexttools

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"unicode/utf8"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/contextkit"
	"github.com/andreylukin/bough/kernel"
)

func init() { kernel.Register("context-tools", func() kernel.Plugin { return plugin{} }) }

type plugin struct{}

func (plugin) Name() string     { return "context-tools" }
func (plugin) Inject() []string { return []string{"agent-tools"} }

func (plugin) Apply(ctx *kernel.Context, _ map[string]any) error {
	reg, err := kernel.Get[agenttools.Registry](ctx, "agent-tools")
	if err != nil {
		return err
	}
	off, err := agenttools.RegisterAll(reg, tools()...)
	if err != nil {
		return fmt.Errorf("context-tools: %w", err)
	}
	ctx.Effect(off)
	return nil
}

func object(required []string, props map[string]any) map[string]any {
	s := agenttools.Object(required, props)
	s["additionalProperties"] = false
	return s
}

func integer(description string, maximum int) map[string]any {
	p := agenttools.Prop("integer", description)
	p["minimum"] = 0
	if maximum > 0 {
		p["maximum"] = maximum
	}
	return p
}

func tools() []agenttools.Tool {
	str := agenttools.Prop
	revision := func() map[string]any {
		return str("string", "Opaque revision returned by inspect or the last successful mutation. A stale revision fails without overwriting newer notes.")
	}
	snapshot := func() map[string]any {
		return str("string", "Immutable snapshot_id from context_inspect. Reuse it for parallel reads/searches and pagination; inspect again if it expires.")
	}
	dry := func() map[string]any {
		return str("boolean", "Validate and report candidate size/hash without committing or creating an archive. A later real commit still checks expected_revision.")
	}
	return []agenttools.Tool{
		bind("context_inspect", "Inspect this session's editable working notes and capture an immutable snapshot. Returns opaque revision, snapshot_id, exact UTF-8 byte/line counts and byte limit; these are not token counts.", object(nil, map[string]any{}), false,
			func(ctx context.Context, cap contextkit.Capability, _ struct{}) (contextkit.Info, error) {
				return cap.Inspect(ctx)
			}),
		bind("context_read", "Read a bounded page from an immutable context snapshot. Offsets and limits are UTF-8 bytes; use next_offset for the next page. Reading does not add the returned notes back into working context.", object([]string{"snapshot_id"}, map[string]any{
			"snapshot_id": snapshot(), "offset": integer("Starting UTF-8 byte offset, default 0.", 0), "limit": integer("Maximum returned bytes, at most 16384; omitted/zero uses the bounded default.", 16384),
		}), false, func(ctx context.Context, cap contextkit.Capability, a contextkit.ReadRequest) (contextkit.ReadResult, error) {
			return cap.Read(ctx, a)
		}),
		bind("context_search", "Find literal text in an immutable context snapshot. Returns bounded match byte ranges and line numbers. Reuse snapshot_id and next_offset for stable pagination while other tools run.", object([]string{"snapshot_id", "query"}, map[string]any{
			"snapshot_id": snapshot(), "query": str("string", "Non-empty literal text; no expression or code execution."), "offset": integer("Resume from the previous result's next_offset; default 0.", 0), "limit": integer("Maximum matches, at most 100; omitted/zero uses the bounded default.", 100),
		}), false, func(ctx context.Context, cap contextkit.Capability, a contextkit.SearchRequest) (contextkit.SearchResult, error) {
			return cap.Search(ctx, a)
		}),
		bind("context_edit", "Apply an atomic ordered batch to this session's working notes. Each half-open UTF-8 byte range addresses the result of the preceding edit; equal start/end inserts, empty text deletes. Validate the complete batch before a short revision-checked commit. On conflict, inspect and rebase; never overwrite blindly.", object([]string{"expected_revision", "edits"}, map[string]any{
			"expected_revision": revision(), "dry_run": dry(), "edits": map[string]any{"type": "array", "minItems": 1, "maxItems": 64, "items": object([]string{"start", "end", "text"}, map[string]any{
				"start": integer("Inclusive UTF-8 byte offset in the current batch candidate.", 0), "end": integer("Exclusive UTF-8 byte offset; must not split a code point.", 0), "text": str("string", "Replacement UTF-8 text, including an empty string for deletion."),
			})},
		}), true, func(ctx context.Context, cap contextkit.Capability, a contextkit.EditRequest) (contextkit.EditResult, error) {
			return cap.Edit(ctx, a)
		}),
		bind("context_offload", "Durably archive an exact excerpt before replacing its live range. Returns an opaque archive_id scoped to this context; keep it for restore. Does not change audit history or private cursor acknowledgements.", object([]string{"expected_revision", "start", "end"}, map[string]any{
			"expected_revision": revision(), "start": integer("Inclusive UTF-8 byte offset in the live notes.", 0), "end": integer("Exclusive UTF-8 byte offset in the live notes.", 0), "replacement": str("string", "Omit to insert an archive-ID marker; supply notes to replace the excerpt, or an explicit empty string to remove it. Archived bytes remain recoverable by archive_id."), "dry_run": dry(),
		}), true, func(ctx context.Context, cap contextkit.Capability, a contextkit.OffloadRequest) (contextkit.OffloadResult, error) {
			return cap.Offload(ctx, a)
		}),
		bind("context_restore", "Insert all or a selected byte range of a verified archive into current working notes. Requires the current revision and this context's archive_id. Preserves intervening notes/events and never rewinds the private cursor.", object([]string{"expected_revision", "archive_id", "offset"}, map[string]any{
			"expected_revision": revision(), "archive_id": str("string", "Opaque ID returned by this context's offload; arbitrary paths and foreign archives are rejected."), "offset": integer("Insertion byte offset in the current live notes.", 0), "start": integer("Optional inclusive byte offset within the archive, default 0.", 0), "end": integer("Optional exclusive archive byte offset; omitted/zero means its end.", 0), "dry_run": dry(),
		}), true, func(ctx context.Context, cap contextkit.Capability, a contextkit.RestoreRequest) (contextkit.EditResult, error) {
			return cap.Restore(ctx, a)
		}),
	}
}

func bind[A, R any](name, description string, schema map[string]any, mutates bool, fn func(context.Context, contextkit.Capability, A) (R, error)) agenttools.Tool {
	return agenttools.Tool{
		Name: name, Description: description, Schema: schema, RequiresContext: true, MutatesContext: mutates,
		Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
			if err := ctx.Err(); err != nil {
				return failure(name, err), nil
			}
			if c.Context == nil {
				return failure(name, errors.New("no editable context capability; use engine-clm")), nil
			}
			args := c.Args
			if len(args) == 0 {
				args = json.RawMessage(`{}`)
			}
			args = bytes.TrimSpace(args)
			if len(args) == 0 || args[0] != '{' || !utf8.Valid(args) {
				return failure(name, errors.New("arguments: expected one UTF-8 JSON object")), nil
			}
			var a A
			dec := json.NewDecoder(bytes.NewReader(args))
			// A typo such as dryrun must fail, never silently become a commit.
			dec.DisallowUnknownFields()
			if err := dec.Decode(&a); err != nil {
				return failure(name, fmt.Errorf("arguments: %w", err)), nil
			}
			if err := dec.Decode(new(any)); err != io.EOF {
				return failure(name, errors.New("arguments: expected one JSON object")), nil
			}
			if err := requiredArguments(args, schema); err != nil {
				return failure(name, err), nil
			}
			out, err := fn(ctx, c.Context, a)
			if err != nil {
				return failure(name, err), nil
			}
			b, err := json.Marshal(out)
			if err != nil {
				return failure(name, err), nil
			}
			return agenttools.Result{Text: string(b), Data: map[string]any{"context_tool": name}}, nil
		},
	}
}

// Providers advertise the schema, but direct/native callers can still omit
// fields. In particular, missing text must not silently become a deletion,
// and null dry_run must not silently turn a proposed edit into a real commit.
func requiredArguments(raw json.RawMessage, schema map[string]any) error {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(raw, &fields); err != nil {
		return fmt.Errorf("arguments: %w", err)
	}
	if required, ok := schema["required"].([]any); ok {
		for _, item := range required {
			key, _ := item.(string)
			if _, exists := fields[key]; !exists {
				return fmt.Errorf("arguments: missing required field %s", key)
			}
		}
	}
	properties, _ := schema["properties"].(map[string]any)
	for key, value := range fields {
		if bytes.Equal(bytes.TrimSpace(value), []byte("null")) {
			return fmt.Errorf("arguments: %s must not be null", key)
		}
		property, _ := properties[key].(map[string]any)
		if property["type"] == "array" {
			var items []json.RawMessage
			if err := json.Unmarshal(value, &items); err != nil {
				return fmt.Errorf("arguments: %w", err)
			}
			if itemSchema, ok := property["items"].(map[string]any); ok {
				for _, item := range items {
					if err := requiredArguments(item, itemSchema); err != nil {
						return err
					}
				}
			}
		}
	}
	return nil
}

func failure(name string, err error) agenttools.Result {
	message := []rune(err.Error())
	if len(message) > 1024 {
		message = append(message[:1024], []rune("… (truncated)")...)
	}
	code := "context_error"
	switch {
	case errors.Is(err, contextkit.ErrConflict):
		code = "revision_conflict"
	case errors.Is(err, contextkit.ErrSnapshotExpired):
		code = "snapshot_expired"
	case errors.Is(err, contextkit.ErrForeignReference):
		code = "foreign_reference"
	case errors.Is(err, contextkit.ErrRecoveryRequired):
		code = "recovery_required"
	case errors.Is(err, context.Canceled):
		code = "cancelled"
	case errors.Is(err, context.DeadlineExceeded):
		code = "deadline_exceeded"
	}
	detail := map[string]any{"error": code, "message": string(message)}
	var conflict *contextkit.Conflict
	if errors.As(err, &conflict) {
		detail["expected_revision"], detail["current_revision"] = conflict.Expected, conflict.Current
	}
	b, _ := json.Marshal(detail)
	return agenttools.Result{Text: string(b), Error: name + ": " + string(message), Data: map[string]any{"context_tool": name, "error_code": code}}
}
