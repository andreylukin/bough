//go:build !windows

package engine

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/unreallabsai/unreal-agent/harness/inbox"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	hsession "github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"

	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/project"
	"github.com/andreylukin/bough/internal/unreal/session"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

const cliUsage = "inspect|reproject [--dry-run]|script [-o file] <session id or history file>"

// Commands is `bough engine` (§10.3): the harness store behind an
// engine session, read without mounting anything. A plugin command
// rather than a launcher case, so main.go only needs its blank import.
func (plugin) Commands() []kernel.Command {
	return []kernel.Command{{
		Name:    "engine",
		Usage:   cliUsage,
		Summary: "an engine-unreal session's harness store: print it, re-project missing history rows, or turn it into an llm-script tape",
		Run: func(cfg map[string]any, args []string) error {
			st, err := readSettings(cfg)
			if err != nil {
				return err
			}
			dir := st.Store
			if dir == "" {
				dir = session.DefaultStore()
			}
			return runCLI(os.Stdout, dir, historyDir(), args)
		},
	}}
}

func historyDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".bough", "history")
}

func runCLI(w io.Writer, storeDir, histDir string, args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: bough engine %s", cliUsage)
	}
	sub, rest := args[0], args[1:]
	fset := flag.NewFlagSet("engine "+sub, flag.ContinueOnError)
	dry := fset.Bool("dry-run", false, "reproject: print the missing rows instead of appending them")
	out := fset.String("o", "", "script: write the tape here instead of stdout")
	if err := fset.Parse(rest); err != nil {
		return err
	}
	if fset.NArg() != 1 {
		return fmt.Errorf("usage: bough engine %s", cliUsage)
	}
	id, histPath := resolve(fset.Arg(0), histDir)
	sid := session.SID(id)
	switch sub {
	case "inspect":
		return inspect(w, storeDir, sid)
	case "reproject":
		return reproject(w, storeDir, sid, histPath, *dry)
	case "script":
		return script(w, storeDir, sid, *out)
	}
	return fmt.Errorf("engine: unknown subcommand %q (want inspect, reproject or script)", sub)
}

// resolve takes a history file path or a session id and returns the
// bough session id and its history file.
func resolve(arg, histDir string) (id, path string) {
	if strings.HasSuffix(arg, ".jsonl") {
		return strings.TrimSuffix(filepath.Base(arg), ".jsonl"), arg
	}
	return arg, filepath.Join(histDir, arg+".jsonl")
}

// items reads a whole session store in order.
func items(storeDir, sid string) ([]sessionstore.Item, error) {
	store, err := localfile.New(storeDir)
	if err != nil {
		return nil, fmt.Errorf("engine: open store %s: %w", storeDir, err)
	}
	var out []sessionstore.Item
	after := sessionstore.BeforeFirst
	for {
		page, err := store.Items(context.Background(), hsession.ID(sid), after, 500)
		if errors.Is(err, fs.ErrNotExist) {
			return nil, fmt.Errorf("engine: no session %s in %s", sid, storeDir)
		}
		if err != nil {
			return nil, fmt.Errorf("engine: read session %s: %w", sid, err)
		}
		out = append(out, page.Items...)
		if !page.More || page.NextAfter <= after {
			return out, nil
		}
		after = page.NextAfter
	}
}

func inspect(w io.Writer, storeDir, sid string) error {
	its, err := items(storeDir, sid)
	if err != nil {
		return err
	}
	for _, it := range its {
		fmt.Fprintf(w, "%5d %s %-16s %s\n", it.Sequence, it.RecordedAt.Format("15:04:05.000"), it.Kind, summary(it))
	}
	return nil
}

// summary is one item on one line: enough to follow a session's turns,
// calls and results without reading its JSON.
func summary(it sessionstore.Item) string {
	switch d := it.Data.(type) {
	case inbox.Input:
		return fmt.Sprintf("%s %s %s", d.ID, d.Kind, clip(string(d.Payload)))
	case hsession.Turn:
		return fmt.Sprintf("turn %s", d.ID)
	case sessionstore.Fork:
		return fmt.Sprintf("forked from %s at %s", d.ParentID, d.PreviousTurnID)
	case sessionstore.ModelResponse:
		var parts []string
		for _, o := range d.Response.Output {
			switch x := o.Data.(type) {
			case ullm.Message:
				parts = append(parts, "text "+clip(x.Text))
			case ullm.ToolCall:
				parts = append(parts, fmt.Sprintf("call %s %s(%s)", x.CallID, x.Name, clip(x.Arguments)))
			case ullm.Reasoning:
				parts = append(parts, "reasoning "+clip(strings.Join(x.Summary, " ")))
			}
		}
		return fmt.Sprintf("%s %s stop=%s: %s", d.TurnID, d.Response.ID, d.Response.Stop, strings.Join(parts, "; "))
	case sessionstore.ToolCallStatus:
		var ops []string
		for _, op := range d.Operations {
			ops = append(ops, fmt.Sprintf("%s:%s", op.ID, op.Status))
		}
		s := fmt.Sprintf("%s waiting=%d ops=[%s]", d.CallID, len(d.Status.WaitingFor), strings.Join(ops, " "))
		if d.Status.Error != "" {
			s += " error=" + clip(d.Status.Error)
		}
		return s
	}
	return ""
}

func clip(s string) string {
	s = strings.Join(strings.Fields(s), " ")
	if r := []rune(s); len(r) > 100 {
		return string(r[:99]) + "…"
	}
	return s
}

// reproject runs the whole store through the projector and records
// (or, dry, prints) every row whose hseq the history file lacks. Tool
// details come from each call's bough.call plan, since no tool rows
// are mounted here.
func reproject(w io.Writer, storeDir, sid, histPath string, dry bool) error {
	its, err := items(storeDir, sid)
	if err != nil {
		return err
	}
	have := map[int64]bool{}
	var h *history.Store
	if dry {
		es, err := history.ReadFile(histPath)
		if err != nil {
			return fmt.Errorf("engine: read %s: %w", histPath, err)
		}
		for _, e := range es {
			if n, ok := hseq(e.Data["hseq"]); ok {
				have[n] = true
			}
		}
	} else {
		if h, err = history.OpenExisting(histPath); err != nil {
			return fmt.Errorf("engine: open %s: %w", histPath, err)
		}
		defer h.Close()
		for _, e := range h.Entries() {
			if n, ok := hseq(e.Data["hseq"]); ok {
				have[n] = true
			}
		}
	}
	p := project.New(project.Config{Render: toolreg.Render, JS: "run_js", RowOutput: 8192})
	n := 0
	for _, it := range its {
		if mr, ok := it.Data.(sessionstore.ModelResponse); ok {
			p.Meta(metaOf(mr.Response.ID))
		}
		for _, o := range p.Item(it) {
			if !o.Record || have[int64(it.Sequence)] {
				continue
			}
			n++
			if dry {
				b, _ := json.Marshal(map[string]any{"kind": o.Kind, "data": o.Data})
				fmt.Fprintln(w, string(b))
				continue
			}
			h.Append(o.Kind, o.Data)
		}
	}
	verb := "appended"
	if dry {
		verb = "missing"
	}
	fmt.Fprintf(os.Stderr, "engine: %d rows %s\n", n, verb)
	return nil
}

// metaOf is what the Gate knew about a response, as far as its id
// says: the Gate names the responses it answered itself.
func metaOf(id string) project.Meta {
	m := project.Meta{ResponseID: id}
	switch {
	case strings.HasPrefix(id, "bough-muted-"):
		m.Muted = true
	case strings.HasPrefix(id, "bough-cancel-"):
		m.Partial = true
	case strings.HasPrefix(id, "bough-error-"):
		m.Err = "provider error"
	}
	return m
}

func hseq(v any) (int64, bool) {
	switch n := v.(type) {
	case float64:
		return int64(n), true
	case int64:
		return n, true
	case int:
		return int64(n), true
	case uint64:
		return int64(n), true
	case json.Number:
		i, err := n.Int64()
		return i, err == nil
	}
	return 0, false
}

// script writes the session's provider responses as an llm-script
// tape, the way to turn a real session into an e2e fixture.
func script(w io.Writer, storeDir, sid, out string) error {
	steps, err := fake.FromStore(storeDir, sid)
	if err != nil {
		return err
	}
	var enc []map[string]any
	skipped := 0
	for _, s := range steps {
		st := tapeStep(s)
		if st == nil {
			skipped++
			continue
		}
		enc = append(enc, st)
	}
	if skipped > 0 {
		// A cancelled request with nothing streamed recorded an empty
		// response, which a tape step cannot say; the replay makes one
		// request fewer for each.
		fmt.Fprintf(os.Stderr, "engine: skipped %d empty responses\n", skipped)
	}
	b, err := json.MarshalIndent(map[string]any{"steps": enc}, "", "  ")
	if err != nil {
		return err
	}
	b = append(b, '\n')
	if out == "" {
		_, err = w.Write(b)
		return err
	}
	return os.WriteFile(out, b, 0o644)
}

// tapeStep is one fake.Step in llm-script's JSON form; nil when the
// step has nothing a tape can hold.
func tapeStep(s fake.Step) map[string]any {
	m := map[string]any{}
	if s.Err != nil {
		m["error"] = "recorded provider error"
		return m
	}
	var text, think []string
	var calls []map[string]any
	for _, it := range s.Output {
		switch d := it.Data.(type) {
		case ullm.Message:
			text = append(text, d.Text)
		case ullm.Reasoning:
			think = append(think, d.Summary...)
		case ullm.ToolCall:
			c := map[string]any{"id": d.CallID, "name": d.Name}
			if json.Valid([]byte(d.Arguments)) {
				c["args"] = json.RawMessage(d.Arguments)
			} else {
				c["args"] = d.Arguments // malformed arguments replay verbatim
			}
			calls = append(calls, c)
		}
	}
	if t := strings.Join(text, "\n\n"); t != "" {
		m["text"] = t
	}
	if t := strings.Join(think, "\n\n"); t != "" {
		m["think"] = t
	}
	if len(calls) > 0 {
		m["calls"] = calls
	}
	switch s.Stop {
	case ullm.StopMaxOutputTokens:
		m["stop"] = "max_output_tokens"
	case ullm.StopRefused:
		m["stop"] = "refused"
	}
	if len(m) == 0 {
		return nil
	}
	if u := s.Usage; u.InputTokens != 0 || u.OutputTokens != 0 {
		m["usage"] = map[string]int64{"input": u.InputTokens, "cached": u.CachedInputTokens, "cache_write": u.CacheWriteInputTokens, "output": u.OutputTokens}
	}
	return m
}
