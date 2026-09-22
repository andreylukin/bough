package tools

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/plugins/llm"
)

// Native tools: the same functions the codemode bindings run, exposed
// to an engine that calls tools natively (docs/unreal-engine.md §8.1).
// They never emit the per-call events of calls.go: under the engine the
// call row is the engine's own, and a second one would double it.
//
// A native call runs on the engine's goroutine under the call's
// context, not codemode's: there is no script, so no script timeout to
// pause and no RunContext to read.

// nativeKey marks a context as a native call's.
type nativeKey struct{}

func native(ctx context.Context) context.Context {
	return context.WithValue(ctx, nativeKey{}, true)
}

func isNative(ctx context.Context) bool {
	v, _ := ctx.Value(nativeKey{}).(bool)
	return v
}

// nativeTools is this row's tool set; write says whether write and
// patch exist (the same rule as their codemode bindings).
func (s *Stats) nativeTools(write bool) []agenttools.Tool {
	idArg := agenttools.Object([]string{"id"}, map[string]any{"id": agenttools.Prop("integer", "the job id")})
	jobDetail := func(args json.RawMessage) string {
		var a struct{ ID int }
		_ = json.Unmarshal(args, &a)
		return "job " + strconv.Itoa(a.ID)
	}
	tools := []agenttools.Tool{
		{
			Name: "bash",
			Description: "Run a shell command (sh) and return its combined output; a non-zero exit fails the call. " +
				"Calls run asynchronously: a slow command keeps running while you do something independent, and its result arrives when it ends. " +
				"For a server, a watcher or anything meant to outlive this turn, pass background: true: you get a job id at once and are told when it exits (or its output matches until). " +
				"timeout caps a foreground command, or a background job's life (seconds, or a duration like \"10m\").",
			Schema: agenttools.Object([]string{"command"}, map[string]any{
				"command":    agenttools.Prop("string", "the script, run by sh"),
				"timeout":    map[string]any{"type": []any{"number", "string"}, "description": "seconds, or a duration like \"10m\""},
				"background": agenttools.Prop("boolean", "run as a background job and return its id at once"),
				"until":      agenttools.Prop("string", "background only: a regexp; you are told as soon as the output matches"),
			}),
			Detail: func(args json.RawMessage) string {
				var a struct{ Command string }
				_ = json.Unmarshal(args, &a)
				return firstLine(a.Command)
			},
			Call: s.nativeBash,
		},
		{
			Name:        "view",
			Description: "Read a file as numbered lines (\"12│text\"), optionally only lines start..end (1-based, inclusive), or list a directory. For an image, call view_image.",
			Schema: agenttools.Object([]string{"path"}, map[string]any{
				"path":  agenttools.Prop("string", "file or directory"),
				"start": agenttools.Prop("integer", "first line, 1-based"),
				"end":   agenttools.Prop("integer", "last line, inclusive"),
			}),
			Detail: func(args json.RawMessage) string {
				var a viewArgs
				_ = json.Unmarshal(args, &a)
				return viewDetail(a.Path, a.rng())
			},
			Call: s.nativeView,
		},
		{
			Name:        "jobs",
			Description: "List the background jobs and their state. It waits up to 10 s for a change first; you are told when a job ends, so never poll.",
			Schema:      agenttools.Object(nil, map[string]any{}),
			Call: func(ctx context.Context, _ agenttools.Call) (agenttools.Result, error) {
				out, err := s.jobs.jobsIn(ctx.Done(), nil)
				return textOrError(out, err), nil
			},
		},
		{
			Name:        "job",
			Description: "One background job's status and the output it has printed so far (waits up to 10 s if it is still running).",
			Schema:      idArg,
			Detail:      jobDetail,
			Call: func(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
				var a struct{ ID int }
				if err := agenttools.Decode("job", c.Args, &a); err != nil {
					return agenttools.Result{}, err
				}
				out, err := s.jobs.jobIn(ctx.Done(), nil, a.ID)
				return textOrError(out, err), nil
			},
		},
		{
			Name:        "job_kill",
			Description: "Stop a background job.",
			Schema:      idArg,
			Detail:      jobDetail,
			Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
				var a struct{ ID int }
				if err := agenttools.Decode("job_kill", c.Args, &a); err != nil {
					return agenttools.Result{}, err
				}
				out, err := s.jobs.jobKill(a.ID)
				return textOrError(out, err), nil
			},
		},
	}
	if write {
		pathDetail := func(args json.RawMessage) string {
			var a struct{ Path string }
			_ = json.Unmarshal(args, &a)
			return a.Path
		}
		tools = append(tools,
			agenttools.Tool{
				Name:        "write",
				Description: "Create or overwrite a whole file, making parent directories. Use it for new files and rewrites, never a shell heredoc.",
				Schema: agenttools.Object([]string{"path", "content"}, map[string]any{
					"path":    agenttools.Prop("string", "the file"),
					"content": agenttools.Prop("string", "the whole new content"),
				}),
				Detail: pathDetail,
				Call:   s.nativeWrite,
			},
			agenttools.Tool{
				Name:        "patch",
				Description: "Replace ONE exact occurrence of old with new in a file. Copy old verbatim from view, with enough lines to be unique. An empty old creates a file that does not exist yet.",
				Schema: agenttools.Object([]string{"path", "old", "new"}, map[string]any{
					"path": agenttools.Prop("string", "the file"),
					"old":  agenttools.Prop("string", "the exact text to replace"),
					"new":  agenttools.Prop("string", "its replacement"),
				}),
				Detail: pathDetail,
				Call:   s.nativePatch,
			},
		)
	}
	return tools
}

// textOrError is a plain tool answer: its text, or its error as the
// call's failure.
func textOrError(out string, err error) agenttools.Result {
	if err != nil {
		return agenttools.Result{Text: out, Error: err.Error()}
	}
	return agenttools.Result{Text: out}
}

type bashArgs struct {
	Command    string `json:"command"`
	Timeout    any    `json:"timeout"`
	Background bool   `json:"background"`
	Until      string `json:"until"`
}

func (s *Stats) nativeBash(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
	var a bashArgs
	if err := agenttools.Decode("bash", c.Args, &a); err != nil {
		return agenttools.Result{}, err
	}
	if strings.TrimSpace(a.Command) == "" {
		return agenttools.Result{Error: "bash: command is empty"}, nil
	}
	data := map[string]any{"cmd": a.Command}
	fail := func(msg string) (agenttools.Result, error) {
		return agenttools.Result{Error: msg, Data: data}, nil
	}
	if a.Until != "" && !a.Background {
		return fail("bash: until watches a background job's output; pass background: true")
	}
	var limit time.Duration
	if a.Timeout != nil {
		d, err := jobLimit(a.Timeout)
		if err != nil {
			return fail(strings.Replace(err.Error(), "limit", "timeout", 1))
		}
		if d <= 0 {
			return fail("bash: timeout must be positive")
		}
		limit = d
	}
	s.mu.Lock()
	policy := s.policy
	s.mu.Unlock()
	if policy != nil {
		if err := policy(a.Command); err != nil {
			return fail(err.Error())
		}
	}
	ctx = native(ctx)
	// The container first: a build still running is neither the
	// command's time nor a job's limit.
	if err := s.project.wait(ctx, nil); err != nil {
		return fail("bash: " + err.Error())
	}
	if a.Background {
		// No foreground grace here, unlike tools.bash(cmd, limit): the
		// engine does not block on a call, so a command that should
		// answer in seconds is a plain foreground call, and one run in
		// the background was asked to outlive the turn.
		if limit == 0 {
			limit = defaultJobLimit
		}
		b, err := s.jobs.start(a.Command, limit, a.Until)
		if err != nil {
			return fail(err.Error())
		}
		data["job"] = b.id
		msg := fmt.Sprintf("job %d started in the background (limit %s): %s", b.id, b.limit, firstLine(a.Command))
		if a.Until != "" {
			msg += fmt.Sprintf("\nwatching its output for %q", a.Until)
		}
		return agenttools.Result{Text: msg + "\nYou will be told when it finishes; job reads its output meanwhile.", Data: data}, nil
	}
	return s.foreground(ctx, c, a.Command, limit, data), nil
}

// foreground runs cmd to its end under ctx (the call's: call_timeout,
// or limit when the model gave one), streaming its output to the call
// row as it arrives.
func (s *Stats) foreground(ctx context.Context, c agenttools.Call, cmd string, limit time.Duration, data map[string]any) agenttools.Result {
	if limit > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, limit)
		defer cancel()
	}
	proc, cleanup, err := s.project.shell(ctx, cmd)
	if err != nil {
		return agenttools.Result{Error: err.Error(), Data: data}
	}
	defer cleanup()
	live, unwatch := s.jobs.watch(c.ID)
	defer unwatch()
	// The stream is redacted before either the row or the live tail sees
	// it, holding back a value split across chunks; the result is
	// redacted whole below, like the codemode path's.
	red := s.project.redactor()
	stream := red.Writer(io.MultiWriter(live, &emitWriter{emit: c.Emit}))
	var buf bytes.Buffer
	out := io.MultiWriter(&buf, stream)
	proc.Stdout, proc.Stderr = out, out
	err = proc.Run()
	stream.Close()
	text := red.String(buf.String())
	if errors.Is(err, exec.ErrWaitDelay) {
		err = nil // a backgrounded child still holds the pipe; the command itself succeeded
	}
	data["exit"] = -1
	switch {
	case ctx.Err() == context.DeadlineExceeded:
		s.exited(-1)
		if limit > 0 {
			return agenttools.Result{Text: text, Error: fmt.Sprintf("bash: killed after %s", limit), Data: data}
		}
		// The engine's call_timeout: boughcall names it.
		return agenttools.Result{Text: text, Error: "bash: " + context.DeadlineExceeded.Error(), Data: data}
	case ctx.Err() != nil:
		s.exited(-1)
		return agenttools.Result{Text: text, Error: "bash: cancelled", Data: data}
	case err != nil:
		code := -1
		if ee, ok := errors.AsType[*exec.ExitError](err); ok {
			code = ee.ExitCode()
		}
		s.exited(code)
		data["exit"] = code
		if code >= 0 {
			return agenttools.Result{Text: text, Error: fmt.Sprintf("exit status %d", code), Data: data}
		}
		return agenttools.Result{Text: text, Error: "bash: " + err.Error(), Data: data}
	}
	s.exited(0)
	data["exit"] = 0
	s.mu.Lock()
	note := s.bashNote
	s.mu.Unlock()
	if note != nil {
		if n := note(cmd); n != "" {
			text += "\n" + n
		}
	}
	return agenttools.Result{Text: text, Data: data}
}

// emitWriter hands output to the call row, never splitting a rune: a
// chunk boundary inside one would reach the row as two U+FFFDs.
type emitWriter struct {
	emit func(string)
	held []byte
}

func (w *emitWriter) Write(p []byte) (int, error) {
	b := append(w.held, p...)
	n := len(b)
	for i := 1; i <= utf8.UTFMax && i <= len(b); i++ {
		if utf8.RuneStart(b[len(b)-i]) {
			if !utf8.FullRune(b[len(b)-i:]) {
				n = len(b) - i
			}
			break
		}
	}
	if n > 0 {
		w.emit(string(b[:n]))
	}
	w.held = append(w.held[:0:0], b[n:]...)
	return len(p), nil
}

type viewArgs struct {
	Path  string `json:"path"`
	Start int    `json:"start"`
	End   int    `json:"end"`
}

// rng is the range as the codemode binding takes it.
func (a viewArgs) rng() []int {
	switch {
	case a.End > 0:
		return []int{a.Start, a.End}
	case a.Start > 0:
		return []int{a.Start}
	}
	return nil
}

func (s *Stats) nativeView(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
	var a viewArgs
	if err := agenttools.Decode("view", c.Args, &a); err != nil {
		return agenttools.Result{}, err
	}
	if a.Path == "" {
		return agenttools.Result{Error: "view: path is required"}, nil
	}
	data := map[string]any{"path": a.Path}
	if llm.ImageMIME(a.Path) != "" {
		// The loop attaches an image from view's marker; a native call
		// has no such seam, and view_image is the tool that does.
		if err := s.project.allowed("view", a.Path); err != nil {
			return agenttools.Result{Error: err.Error(), Data: data}, nil
		}
		if _, err := os.Stat(a.Path); err != nil {
			return agenttools.Result{Error: withNeighbours(a.Path, err).Error(), Data: data}, nil
		}
		return agenttools.Result{Text: a.Path + " is an image; call view_image with this path to see it.", Data: data}, nil
	}
	out, err := s.viewFile(a.Path, a.rng()...)
	r := textOrError(out, err)
	r.Data = data
	return r, nil
}

func (s *Stats) nativeWrite(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
	var a struct {
		Path    string `json:"path"`
		Content string `json:"content"`
	}
	if err := agenttools.Decode("write", c.Args, &a); err != nil {
		return agenttools.Result{}, err
	}
	if a.Path == "" {
		return agenttools.Result{Error: "write: path is required"}, nil
	}
	data := map[string]any{"path": a.Path}
	_, statErr := os.Stat(a.Path)
	out, err := s.writeFile(native(ctx), a.Path, a.Content)
	if err != nil {
		return agenttools.Result{Error: err.Error(), Data: data}, nil
	}
	data["add"], data["del"] = diffCounts(out)
	if statErr != nil { // a new file: every line is added, and there is no diff to count
		data["add"] = lineCount(a.Content)
	}
	return agenttools.Result{Text: out, Data: data}, nil
}

func (s *Stats) nativePatch(ctx context.Context, c agenttools.Call) (agenttools.Result, error) {
	var a struct {
		Path string `json:"path"`
		Old  string `json:"old"`
		New  string `json:"new"`
	}
	if err := agenttools.Decode("patch", c.Args, &a); err != nil {
		return agenttools.Result{}, err
	}
	if a.Path == "" {
		return agenttools.Result{Error: "patch: path is required"}, nil
	}
	data := map[string]any{"path": a.Path}
	out, err := s.patchFile(native(ctx), a.Path, a.Old, a.New)
	if err != nil {
		return agenttools.Result{Error: err.Error(), Data: data}, nil
	}
	data["add"], data["del"] = diffCounts(out)
	return agenttools.Result{Text: out, Data: data}, nil
}
