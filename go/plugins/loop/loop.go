// Package loop is the agent loop plugin: it owns the conversation,
// provides the "inputs" channel, the "runner" service and the
// "prompt-sections" registry (see Sections), and drives the codemode
// loop (llm -> extract js -> run -> feed back).
//
// Conversation state lives in history entries (the optional "history"
// service makes them durable; absent, a process-local list is used).
// Model messages are derived from entries by the optional "projection"
// service (DefaultProject otherwise), and the system prompt may be
// transformed by the optional "cognition" service.
package loop

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Message is one turn of conversation. The seam type is owned by
// plugins/llm; this alias keeps the two packages nominally identical
// so kernel.Get[LLM] succeeds (llm does not import loop, so no cycle).
type Message = llm.Message

// LLM is the "llm" service seam.
type LLM = llm.LLM

// Codemode is the "codemode" service seam.
type Codemode interface {
	RegisterTool(name string, fn any)
	Run(code string) (string, error)
}

// Hooks is the optional "hooks" service seam. Fire runs every hook
// file for event and returns the merged result object (nil if none).
type Hooks interface {
	Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error)
}

// Skills is the optional "skills" service seam. Inject returns
// ready-formatted "[skill: <name>]\n<body>" blocks for skills whose
// name is mentioned in the human input.
type Skills interface {
	Inject(input string) []string
}

// SystemContext is the optional "context-md" service seam. Preamble
// is prepended to the system prompt at the start of every turn, so a
// context file created or edited mid-session is picked up.
type SystemContext interface {
	Preamble() string
}

// TurnStats is the optional "turn-stats" service seam (tools-basic):
// files written and the last bash exit code since the previous Take.
// Stamped onto the "done" entry as data {"files": [...], "exit": n}
// ("exit" only when a bash call ran this turn).
type TurnStats interface {
	Take() (files []string, exit int, ran bool)
}

// Checkpointer is the optional "checkpoints" service seam (history):
// Snapshot records the working tree before a turn ("" when there is
// no git repo to snapshot) and Pin names the tree for the turn's seq,
// which is what /undo reverts against.
type Checkpointer interface {
	Snapshot() string
	Pin(seq int64, tree string)
}

// Sections is the "prompt-sections" service: named system-prompt
// sections that plugins register (workers documents tools.spawn, mcp
// its bound tools). The loop appends every section, sorted by name,
// to the system prompt on each model call. Safe for concurrent use.
type Sections struct {
	mu sync.Mutex
	m  map[string]string
}

// Set registers (or replaces) the section named name; empty text
// removes it.
func (s *Sections) Set(name, text string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.m == nil {
		s.m = map[string]string{}
	}
	if text == "" {
		delete(s.m, name)
		return
	}
	s.m[name] = text
}

// TextExcept is Text without the named sections: a subagent must not
// be handed the advert for a tool it is forbidden to call.
func (s *Sections) TextExcept(skip ...string) string {
	if s == nil {
		return ""
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	names := slices.Sorted(maps.Keys(s.m))
	parts := make([]string, 0, len(names))
	for _, n := range names {
		if slices.Contains(skip, n) {
			continue
		}
		parts = append(parts, s.m[n])
	}
	return strings.Join(parts, "\n\n")
}

// Text joins all sections, sorted by name, with blank lines.
func (s *Sections) Text() string {
	if s == nil {
		return ""
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	names := slices.Sorted(maps.Keys(s.m))
	parts := make([]string, 0, len(names))
	for _, n := range names {
		parts = append(parts, s.m[n])
	}
	return strings.Join(parts, "\n\n")
}

// History is the optional "history" service seam: append-only session
// record. Absent, the loop keeps a process-local in-memory list.
type History interface {
	Append(kind string, data map[string]any) history.Entry
	Entries() []history.Entry
	Path() string
}

// Cognition is the optional "cognition" service seam: transforms or
// replaces the built default system prompt.
type Cognition interface {
	System(base string) string
}

// Projection is the optional "projection" service seam: derives the
// model messages from the history entries each step.
type Projection interface {
	Project(entries []history.Entry) []llm.Message
}

// Event is the payload emitted on "loop/event".
// Kind is one of: "assistant-delta" (a streamed fragment, not recorded),
// "assistant", "code", "result", "error", "done", "steer" (a mid-turn
// user message landed; recorded as an "input" entry) from
// the loop itself; other plugins may emit further kinds (e.g. workers'
// "sub:*" subagent events). Data carries optional extra payload (e.g.
// {"worker": N} on sub:* events); nil for the loop's own events.
type Event struct {
	Kind string
	Text string
	Data map[string]any
}

// defaultMaxSteps caps model steps per turn; the loop row's `max_steps`
// config raises it (a benchmark turn is one long task).
const defaultMaxSteps = 100
const maxResultBytes = 8 * 1024

// SystemPrompt is the base identity and tool catalogue every agent in
// this process shares; a subagent (workers) starts from the same text.
const SystemPrompt = `You are bough, a coding agent. You act by writing JavaScript
in fenced code blocks:

` + "```js" + `
console.log(tools.bash("ls"))
` + "```" + `

Available in the runtime:
- tools.bash(cmd) -> string: run a shell command, returns its output.
  It is killed after 60 s (the error says so); split long work into
  shorter commands or background it yourself.
- tools.view(path, [start, end]) -> string: a file's lines, numbered ("12│text"); optional 1-based inclusive range
- tools.write(path, content) -> string: create or overwrite a whole file (use this for new files and rewrites, never a shell heredoc)
- tools.patch(path, old, new) -> string: replace ONE exact occurrence of old with new (copy old verbatim from view, enough lines to be unique)
- console.log(...): print; everything printed is returned to you

The runtime is synchronous: there is no event loop, and async, await
and Promise are SYNTAX ERRORS that kill the whole block. Every tool
returns its value directly — write tools.bash("ls"), not await. To do
several things, call them one after another or map over a list.

Each code block you write is executed and its output is sent back to you
as the next message. Declarations (const/let/var) do not persist between
blocks; print what you need to carry over. Never write output or result
blocks yourself; only the runtime returns output. Take as many steps as
you need. When you are done, reply with plain text only — no code block —
and that ends the turn.

A reply without a code block ENDS the turn, whatever it says. Never
announce what you will do next ("I'll verify…", "Next, let me…") without
the code block that does it in the same reply. Before you finish, run the
task's own checks (its tests, a build, the command the brief names) and
fix what they show; end only when the work is actually done or you have
hit a wall, and say which.

Be thorough over broad briefs: re-read the brief before finishing and
check each requirement against a change you made. When it says every
module or utility has defects, find a concrete defect in each one and
fix it against the textbook definition — a comment in the code that
names a shortcut ("biased", "approximate", "TODO") is a defect, not a
design choice. Make that concrete: early on, grep the code you are
fixing for TODO|FIXME|XXX|HACK|biased|approx|simplif|naive|placeholder|
for now, print the hits, and carry that list to the end — each hit is a
defect to fix (or to rule out with a reason in your final reply). Your final reply lists each defect as file: what was
wrong → what you changed; never claim a fix you did not make.

Fix in place: keep every public interface as it is — function names and
signatures, return types and shapes, dict keys, CLI flags and exit codes,
file formats — unless the brief asks you to change it. Hidden checks call
the code the way the original did; a scalar that becomes an array or a
renamed key fails them even when the math is right. Add, do not rename.
A fix is the default behaviour: never gate it behind a new opt-in flag
or parameter that leaves the old, wrong path as what callers get. And
never revert a textbook-correct fix because it exposes a symptom
downstream (a correct estimator that can dip slightly below zero, a
stricter check that now fires): keep the correct formula and fix the
consumer — clamp, threshold, or handle the case where it is used.`

// askPromptSection documents tools.ask; appended to the system prompt
// only when an "ask-answers" service (the ask plugin) is mounted. The
// options nudge matters: options inlined into the question string
// render as plain text, separate arguments render as clickable option
// rows in the UI.
const askPromptSection = `You may ask the user a question from code: tools.ask(question, ...options) -> string blocks until they answer and returns the answer. Pass each option as a separate argument — tools.ask(question, opt1, opt2, ...) — so they render as clickable choices; never inline the options into the question text.`

var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// anyBlock matches every fenced block; group 1 is the info string.
var anyBlock = regexp.MustCompile("(?s)```([^\\s`]*)[^\n]*\n.*?```")

const removedBlock = "[guessed output omitted]"

// errEmptyReply is the turn error after two empty replies in a row.
var errEmptyReply = errors.New("the model returned an empty reply twice (provider hiccup) — send again, or switch with /model")

// outputTags are the fence info strings a model uses for invented
// results. A bare fence counts too. Anything else (```python,
// ```yaml, ```diff …) is code the model wants the user to see and
// stays.
var outputTags = map[string]bool{"": true, "output": true, "text": true, "txt": true,
	"plaintext": true, "console": true, "stdout": true, "stderr": true, "result": true, "log": true}

// stripFakeBlocks replaces every output-looking fenced block
// (```output, ```text, bare ```...) in an assistant reply with
// removedBlock: those are model-guessed results, and left in the
// transcript they render like real runtime output. Language-tagged
// fences are kept: they are code being shown, not results.
func stripFakeBlocks(reply string) string {
	return anyBlock.ReplaceAllStringFunc(reply, func(m string) string {
		tag := anyBlock.FindStringSubmatch(m)[1]
		if tag == "js" || !outputTags[strings.ToLower(tag)] {
			return m
		}
		return removedBlock
	})
}

// DefaultProject is the built-in history -> model-messages projection:
// input -> user, assistant -> assistant, result -> user "[tool output]\n...",
// and a "!" command entry (the ui's bash mode) paired with its following
// "system" output entry -> user "[shell]\n$ cmd\noutput". "/" command
// entries are UI-only. Other kinds (code, error, done) carry no
// model-visible text. Pure: no state, entries in -> messages out.
// cancelledNote is what a cancelled turn projects to.
const cancelledNote = "[cancelled] The user interrupted this turn. Do not resume or redo its work unless asked again."

// undoPrefix opens the note an "undo" entry projects to; the reverted
// paths follow.
const undoPrefix = "[undo] The user reverted these files to their content from before the turn that wrote them: "

func DefaultProject(entries []history.Entry) []llm.Message {
	var msgs []llm.Message
	for i := 0; i < len(entries); i++ {
		e := entries[i]
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "input":
			msgs = append(msgs, llm.Message{Role: "user", Content: text})
		case "assistant":
			msgs = append(msgs, llm.Message{Role: "assistant", Content: text})
		case "result":
			msgs = append(msgs, llm.Message{Role: "user", Content: "[tool output]\n" + text})
		case "cancelled":
			// The user interrupted the turn. Without this the killed
			// request sits in context looking merely unfinished, and a
			// later unrelated question resumed it (re-running the slow
			// command that was cancelled). Folded into the preceding
			// user message when there is one, so roles keep alternating.
			if n := len(msgs); n > 0 && msgs[n-1].Role == "user" {
				msgs[n-1].Content += "\n\n" + cancelledNote
			} else {
				msgs = append(msgs, llm.Message{Role: "user", Content: cancelledNote})
			}
		case "undo":
			// /undo put files back; the model's "wrote X" results are
			// stale, so tell it (folded like the cancelled note).
			var files []string
			switch l := e.Data["files"].(type) {
			case []string:
				files = l
			case []any:
				for _, x := range l {
					if s, ok := x.(string); ok {
						files = append(files, s)
					}
				}
			}
			if len(files) == 0 {
				continue
			}
			undoNote := undoPrefix + strings.Join(files, ", ")
			if n := len(msgs); n > 0 && msgs[n-1].Role == "user" {
				msgs[n-1].Content += "\n\n" + undoNote
			} else {
				msgs = append(msgs, llm.Message{Role: "user", Content: undoNote})
			}
		case "command":
			if !strings.HasPrefix(text, "!") {
				continue
			}
			cmd := strings.TrimSpace(text[1:])
			out := ""
			if i+1 < len(entries) && entries[i+1].Kind == "system" {
				out, _ = entries[i+1].Data["text"].(string)
				i++
			}
			msgs = append(msgs, llm.Message{Role: "user", Content: "[shell]\n$ " + cmd + "\n" + out})
		}
	}
	return msgs
}

// memHistory is the fallback History when no "history" service is
// mounted: same contract, process-local, gone at exit.
type memHistory struct {
	mu      sync.Mutex
	entries []history.Entry
}

func (m *memHistory) Append(kind string, data map[string]any) history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	e := history.Entry{Seq: int64(len(m.entries) + 1), At: time.Now(), Kind: kind, Data: data}
	m.entries = append(m.entries, e)
	return e
}

func (m *memHistory) Entries() []history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([]history.Entry(nil), m.entries...)
}

func (m *memHistory) Path() string { return "" }

// runner implements the "runner" service. hooks, skills, sysctx, cog
// and proj are optional seams; nil means built-in behavior. hist is
// never nil (memHistory fallback).
type runner struct {
	mu       sync.Mutex
	llm      LLM
	code     Codemode
	maxSteps int // model steps per turn; 0 = defaultMaxSteps
	// noteData is the extra data of the history entry being emitted
	// (the done entry's files/exit), set around the emit call so the
	// mount's publisher can attach it to the live event. Run holds mu
	// for the whole turn, so a plain field is enough.
	noteData map[string]any
	hooks    Hooks
	skills   Skills
	sysctx   SystemContext
	hist     History
	cog      Cognition
	proj     Projection
	stats    TurnStats
	cp       Checkpointer
	secs     *Sections
	hasAsk   bool // an "ask-answers" service is mounted: document tools.ask
	// steer hands over the steering messages sent since the last
	// call (turns.takeSteers); final shuts the gate, see there. nil =
	// no steering.
	steer   func(final bool) []string
	system  string
	started bool
}

// note appends a history entry and emits the matching event.
func (r *runner) note(emit func(kind, text string), kind, text string, extra map[string]any) {
	data := map[string]any{"text": text}
	maps.Copy(data, extra)
	r.hist.Append(kind, data)
	r.noteData = extra
	emit(kind, text)
	r.noteData = nil
}

// admit runs one user line through the user-prompt-submit hook (which
// may rewrite or block it), expands @files, injects skills and records
// the "input" entry. Returns the line as admitted and, when the hook
// blocked it, its reason (nothing recorded then).
func (r *runner) admit(ctx context.Context, input string, steer bool, emit func(kind, text string)) (line, blocked string) {
	if res := r.fire(ctx, "user-prompt-submit", map[string]any{"input": input}, emit); res != nil {
		if b, ok := res["block"].(string); ok {
			return input, b
		}
		if in, ok := res["input"].(string); ok {
			input = in
		}
	}
	var msg strings.Builder
	msg.WriteString(input)
	for _, block := range ExpandAt(input, ".") {
		msg.WriteString("\n\n" + block)
	}
	if r.skills != nil {
		for _, block := range r.skills.Inject(input) {
			msg.WriteString("\n\n" + block)
		}
	}
	data := map[string]any{"text": msg.String()}
	if steer {
		data["steer"] = true
	}
	// The turn's checkpoint: the working tree as it was before the
	// model touched anything, on the turn's input entry and pinned
	// by seq (a steer lands mid-turn: no checkpoint of its own).
	tree := ""
	if !steer && r.cp != nil {
		if tree = r.cp.Snapshot(); tree != "" {
			data["checkpoint"] = tree
		}
	}
	in := r.hist.Append("input", data)
	if tree != "" {
		r.cp.Pin(in.Seq, tree)
	}
	return input, ""
}

// landSteers admits every pending steering message like any input
// (hook, @files, skills; an "input" entry the projection shows as a
// user message) and announces each as a "steer" event — a blocked one
// too, followed by the hook's reason as an error, so the ui stops
// showing it pending. True when any was admitted: the caller then
// drops the rest of the current reply and goes straight back to the
// model. final is the turn's last boundary: the take shuts the steer
// gate, so a message arriving behind it is refused and its sender
// queues it as ordinary input, never stranded.
func (r *runner) landSteers(ctx context.Context, emit func(kind, text string), final bool) bool {
	if r.steer == nil {
		return false
	}
	landed := false
	for _, text := range r.steer(final) {
		emit("steer", text)
		if _, blocked := r.admit(ctx, text, true, emit); blocked != "" {
			r.note(emit, "error", blocked, nil)
			continue
		}
		landed = true
	}
	return landed
}

// doneData builds the "done" entry's data: files written this turn and
// the last bash exit code (when a bash call ran), from the optional
// turn-stats seam. Without it, only "files": [] is present.
func (r *runner) doneData() map[string]any {
	data := map[string]any{"files": []string{}}
	if r.stats == nil {
		return data
	}
	files, exit, ran := r.stats.Take()
	if files == nil {
		files = []string{}
	}
	data["files"] = files
	if ran {
		data["exit"] = exit
	}
	return data
}

// fire runs a hook event if a hooks service is present. A Fire error
// is logged as a loop error event and treated as no-op, never fatal.
func (r *runner) fire(ctx context.Context, event string, payload map[string]any, emit func(kind, text string)) map[string]any {
	if r.hooks == nil {
		return nil
	}
	res, err := r.hooks.Fire(ctx, event, payload)
	if err != nil {
		emit("error", "hook "+event+": "+err.Error())
		return nil
	}
	return res
}

// complete asks the model for the next step. A streaming provider
// (llm.Streamer) reports each fragment as an "assistant-delta" event so
// the ui can show the reply as it forms; only the finished reply is
// recorded to history, as before, so deltas never reach the model.
func (r *runner) complete(ctx context.Context, sys string, emit func(kind, text string)) (string, error) {
	return r.completeMsgs(ctx, sys, r.project(), emit)
}

func (r *runner) completeMsgs(ctx context.Context, sys string, msgs []Message, emit func(kind, text string)) (string, error) {
	if st, ok := r.llm.(llm.Streamer); ok {
		return st.Stream(ctx, sys, msgs, func(delta string) { emit("assistant-delta", delta) })
	}
	return r.llm.Complete(ctx, sys, msgs)
}

// outOfSteps is the last thing the model is asked when the step budget
// runs out. A turn that spent every step on tools still owes the user
// an answer: what was found, and what is left.
const outOfSteps = `You are out of steps for this turn. Do not write a code block — nothing more will run. Reply now, as plain text, with what you found and what you would do next. If you never reached an answer, say so plainly and name the obstacle.`

// atMaxBytes caps one attached file so a stray "@big.log" cannot
// swallow the context.
const atMaxBytes = 64 * 1024

var atRef = regexp.MustCompile(`(?:^|\s)@([^\s@]+)`)

// ExpandAt turns every "@path" word in input that names a regular
// file under root into an attachment block "[file: path]\n<contents>",
// in order of first mention, each path once. Words that are not files
// are left alone (an "@handle" is just text). Pure apart from the reads.
func ExpandAt(input, root string) []string {
	var blocks []string
	seen := map[string]bool{}
	for _, m := range atRef.FindAllStringSubmatch(input, -1) {
		p := strings.TrimRight(m[1], ".,;:)")
		if seen[p] || strings.Contains(p, "..") {
			continue
		}
		full := filepath.Join(root, p)
		st, err := os.Stat(full)
		if err != nil || !st.Mode().IsRegular() {
			continue
		}
		seen[p] = true
		data, err := os.ReadFile(full)
		if err != nil {
			continue
		}
		note := ""
		if len(data) > atMaxBytes {
			data = data[:atMaxBytes]
			note = fmt.Sprintf("\n[truncated at %d bytes of %d; use tools.view for the rest]", atMaxBytes, st.Size())
		}
		blocks = append(blocks, "[file: "+p+"]\n"+string(data)+note)
	}
	return blocks
}

// project derives this step's model messages from the history entries.
// noneNoted names an empty result. A block that computes a value and
// never prints it comes back as a bare "[tool output]" — which a model
// reads as a broken runtime, not as its own missing console.log. One
// session spent its whole budget probing "why do the tools return
// nothing" and then told the user the repository tools were down.
func noneNoted(out string) string {
	if strings.TrimSpace(out) == "" {
		return "(the block ran and printed nothing — console.log a value to see it)"
	}
	return out
}

// envSection tells the model what it would otherwise spend a step
// discovering — and get wrong. Without it a turn opens with `pwd`,
// reaches for GNU flags on a BSD userland, and invents absolute paths.
func envSection() string {
	wd, err := os.Getwd()
	if err != nil {
		wd = "unknown"
	}
	s := "Environment:\n- Working directory: " + wd +
		"\n  Paths you write are relative to it. Never guess an absolute path; list a directory instead.\n- Platform: " + runtime.GOOS
	if runtime.GOOS == "darwin" {
		s += " — BSD userland, not GNU: find has no -printf, sed -i takes an argument, stat uses -f. Prefer portable flags."
	}
	return s
}

func (r *runner) project() []Message {
	entries := r.hist.Entries()
	if r.proj != nil {
		return r.proj.Project(entries)
	}
	return DefaultProject(entries)
}

func (r *runner) Run(ctx context.Context, input string, emit func(kind, text string)) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	note := func(kind, text string, extra map[string]any) { r.note(emit, kind, text, extra) }
	// finish ends the turn: steers sent since the last boundary land
	// first (history the next turn sees; under a cancelled turn's
	// [cancelled] note, so nothing runs on its own) and the gate
	// shuts, then the marker (if any) and the done.
	finish := func(marker string, extra map[string]any) {
		r.landSteers(ctx, emit, true)
		if marker != "" {
			note(marker, "", nil)
		}
		note("done", "", extra)
	}

	if !r.started {
		r.started = true
		r.system = SystemPrompt
		if r.hasAsk {
			r.system += "\n\n" + askPromptSection
		}
		if res := r.fire(ctx, "session-start", map[string]any{}, emit); res != nil {
			if c, ok := res["context"].(string); ok && c != "" {
				r.system += "\n\n" + c
			}
		}
	}
	// Per turn: where we are (the cwd can change between turns), the
	// context files re-read (they may have appeared or changed
	// mid-session) and the live plugin prompt sections.
	system := envSection() + "\n\n" + r.system
	if r.sysctx != nil {
		if p := r.sysctx.Preamble(); p != "" {
			system = p + "\n\n" + system
		}
	}
	if s := r.secs.Text(); s != "" {
		system += "\n\n" + s
	}

	input, blocked := r.admit(ctx, input, false, emit)
	if blocked != "" {
		note("error", blocked, nil)
		finish("", r.doneData()) // end the turn so headless drain sees it
		return nil
	}

	maxSteps := r.maxSteps
	if maxSteps <= 0 {
		maxSteps = defaultMaxSteps
	}
	retried := false
	for step := 0; step < maxSteps; step++ {
		r.landSteers(ctx, emit, false) // a steer sent during the last block joins the context now
		sys := system
		if r.cog != nil {
			sys = r.cog.System(sys)
		}
		reply, err := r.complete(ctx, sys, emit)
		if ctx.Err() != nil {
			finish("cancelled", r.doneData()) // what it wrote so far: /undo after esc reverts it
			return ctx.Err()
		}
		if err == nil && strings.TrimSpace(reply) == "" {
			// A provider hiccup (a stream that ends with no content)
			// gets one silent retry; a second empty reply is an error
			// the user can act on, never a blank assistant entry.
			if !retried {
				retried = true
				step--
				continue
			}
			err = errEmptyReply
		}
		if err != nil {
			note("error", err.Error(), nil)
			finish("", r.doneData()) // every turn ends with a done, even on llm failure
			return err
		}
		retried = false
		reply = stripFakeBlocks(reply)
		note("assistant", reply, nil)
		blocks := jsBlock.FindAllStringSubmatch(reply, -1)
		if len(blocks) == 0 {
			// A steer sent while the model wrote this final reply
			// lands inside the turn: ask again, no done yet. (The
			// final take shut the gate; the next boundary's take
			// reopens it.)
			if r.landSteers(ctx, emit, true) {
				continue
			}
			note("done", "", r.doneData())
			r.fire(ctx, "stop", map[string]any{"input": input, "reply": reply}, emit)
			return nil
		}
		for _, m := range blocks {
			if r.landSteers(ctx, emit, false) {
				break // steered: the rest of this reply is stale, ask again
			}
			code := m[1]
			if res := r.fire(ctx, "pre-code-exec", map[string]any{"code": code}, emit); res != nil {
				if reason, ok := res["deny"].(string); ok {
					note("result", "[hook denied: "+reason+"]", map[string]any{"code": code})
					continue
				}
				if c, ok := res["code"].(string); ok {
					code = c
				}
			}
			note("code", code, nil)
			out, runErr := r.runCode(ctx, code)
			if ctx.Err() != nil {
				finish("cancelled", nil)
				return ctx.Err()
			}
			// Whatever the block printed BEFORE it failed is kept: a
			// block that logged a finished subagent's report and then
			// threw on the next call must not lose the report.
			if runErr != nil {
				if out = strings.TrimRight(out, "\n"); out != "" {
					out += "\n"
				}
				out += "error: " + runErr.Error()
			}
			out = truncate(noneNoted(out), maxResultBytes)
			if res := r.fire(ctx, "post-result", map[string]any{"code": code, "result": out}, emit); res != nil {
				if s, ok := res["result"].(string); ok {
					out = s
				}
			}
			// A run error still lands as a "result" entry (text
			// "error: ...") so the projection feeds it back to the
			// model; the UI event keeps the "error" kind.
			r.hist.Append("result", map[string]any{"text": out, "code": code})
			if runErr != nil {
				emit("error", out)
			} else {
				emit("result", out)
			}
		}
	}
	// The budget is spent. Ending here would leave the user with tool
	// output and no answer, so the last call buys one: no blocks run,
	// whatever comes back is the turn's reply.
	note("system", fmt.Sprintf("step budget spent (%d steps); asking for a final answer", maxSteps), nil)
	msgs := append(r.project(), Message{Role: "user", Content: outOfSteps})
	reply, err := r.completeMsgs(ctx, system, msgs, emit)
	if ctx.Err() != nil {
		finish("cancelled", nil)
		return ctx.Err()
	}
	if err != nil {
		note("error", err.Error(), nil)
		finish("", r.doneData())
		return err
	}
	reply = strings.TrimSpace(jsBlock.ReplaceAllString(stripFakeBlocks(reply), ""))
	note("assistant", reply, nil)
	finish("", r.doneData())
	r.fire(ctx, "stop", map[string]any{"input": input, "reply": reply}, emit)
	return nil
}

// toInt reads a yaml int, float, or --set string.
func toInt(v any) (int, error) {
	switch n := v.(type) {
	case int:
		return n, nil
	case int64:
		return int(n), nil
	case float64:
		if n == float64(int(n)) {
			return int(n), nil
		}
	case string:
		return strconv.Atoi(n)
	}
	return 0, fmt.Errorf("not an integer: %v", v)
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	// Never cut inside a multi-byte rune: the tail would be invalid
	// UTF-8 for the model and for anything streaming our output.
	for n > 0 && !utf8.RuneStart(s[n]) {
		n--
	}
	return s[:n] + "\n[truncated]"
}

type plugin struct{}

func init() {
	kernel.Register("loop", func() kernel.Plugin { return &plugin{} })
}

func (p *plugin) Name() string     { return "loop" }
func (p *plugin) Inject() []string { return []string{"llm", "codemode"} }

func (p *plugin) Apply(kctx *kernel.Context, cfg map[string]any) error {
	llm, err := kernel.Get[LLM](kctx, "llm")
	if err != nil {
		return err
	}
	code, err := kernel.Get[Codemode](kctx, "codemode")
	if err != nil {
		return err
	}
	r := &runner{llm: llm, code: code, hist: &memHistory{}, secs: &Sections{}}
	if v, ok := cfg["max_steps"]; ok {
		n, err := toInt(v)
		if err != nil || n < 1 {
			return fmt.Errorf("loop: max_steps must be a positive integer, got %v", v)
		}
		r.maxSteps = n
	}
	kctx.Provide("prompt-sections", r.secs)
	// Optional seams: absent services are a clean no-op / built-in.
	if h, err := kernel.Get[Hooks](kctx, "hooks"); err == nil {
		r.hooks = h
	}
	if s, err := kernel.Get[Skills](kctx, "skills"); err == nil {
		r.skills = s
	}
	if sc, err := kernel.Get[SystemContext](kctx, "context-md"); err == nil {
		r.sysctx = sc
	}
	if h, err := kernel.Get[History](kctx, "history"); err == nil {
		r.hist = h
	}
	if c, err := kernel.Get[Cognition](kctx, "cognition"); err == nil {
		r.cog = c
	}
	if pr, err := kernel.Get[Projection](kctx, "projection"); err == nil {
		r.proj = pr
	}
	if _, err := kernel.Get[any](kctx, "ask-answers"); err == nil {
		r.hasAsk = true
	}
	if st, err := kernel.Get[TurnStats](kctx, "turn-stats"); err == nil {
		r.stats = st
	}
	if c, err := kernel.Get[Checkpointer](kctx, "checkpoints"); err == nil {
		r.cp = c
	}
	kctx.Provide("runner", r)

	inputs := make(chan string, 8)
	kctx.Provide("inputs", inputs)

	t := &turns{}
	kctx.Provide("cancel", t.Cancel)
	r.steer = t.takeSteers
	kctx.Provide("steer", t.Steer)

	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		for input := range inputs {
			t.run(ctx, r, input, func(kind, text string) {
				// Live gets what replay gets: a noted entry's data
				// (the done marker's files and exit) rides along.
				kctx.Emit("loop/event", Event{Kind: kind, Text: text, Data: r.noteData})
			})
		}
	}()
	kctx.Effect(func() {
		cancel()
		close(inputs)
	})
	return nil
}
