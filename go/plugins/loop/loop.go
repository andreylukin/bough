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

	"github.com/andreylukin/bough/internal/schema"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/contextmd"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"unicode"
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

// cataloguer is codemode's optional prompt-catalogue seam: the tool
// list the model is shown is generated from the tools actually
// registered, so it cannot drift from what is mounted (it nearly did
// when background jobs landed and the hand-written list did not know).
type cataloguer interface {
	Catalogue() string
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

// loadedLister is the optional half of the context-md seam: the files
// Preamble is currently reading, so each can be announced by name. A
// provider without it is announced as one "context files" piece.
type loadedLister interface {
	Loaded() []string
}

// contextParter is the richer half: what each file actually
// contributed after de-duplication, so the context row shows the text
// that went in rather than the file on disk.
type contextParter interface {
	Parts() []contextmd.Part
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

// Names is the registered section names, sorted — the order Text
// joins them in.
func (s *Sections) Names() []string {
	if s == nil {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Sorted(maps.Keys(s.m))
}

// Get is one section's text, "" when it is not registered.
func (s *Sections) Get(name string) string {
	if s == nil {
		return ""
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.m[name]
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

// Notices is the optional "job-notices" service seam (tools-basic's
// background jobs): Take drains the notices a finished job queued, and
// Wake fires when one arrives so an idle agent starts a turn on it.
type Notices interface {
	Take() []string
	Wake() <-chan struct{}
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
const maxResultBytes = 64 * 1024

// SystemPrompt is the base identity and tool catalogue every agent in
// this process shares; a subagent (workers) starts from the same text.
const SystemPrompt = `You are bough, a coding agent. You act by writing JavaScript
in fenced code blocks:

` + "```js" + `
console.log(tools.bash("ls"))
` + "```" + `

Only a fence tagged exactly ` + "`js`" + ` runs. Not ` + "`javascript`" + `, not a
<script> tag, not bare code in prose — anything else is read as text,
nothing happens, and you are asked again. You have no JSON tool-calling
interface here: a reply like {"cmd": "ls"} or {"name": …, "arguments":
…} calls nothing. The fenced program IS the tool call.

The runtime is JavaScript but it is NOT Node: no require, no import, no
fs, no fetch, no process, no Buffer, no npm. Everything you can reach
is tools.* and console.log. It is also synchronous: there is no event
loop, and async, await and Promise are SYNTAX ERRORS that kill the
whole block. Every tool
returns its value directly — write tools.bash("ls"), not await. To do
several things, call them one after another or map over a list.

Write ONE code block per reply: only the first block runs, anything
after it is dropped. That block is executed and its output is sent back
to you as the next message. Do not write the next command before you
have seen the output of this one — put several steps in ONE program
instead when they belong together. Declarations (const/let/var) do not
persist between blocks; print what you need to carry over. Never write
output or result blocks yourself; only the runtime returns output. Take
as many steps as you need.

A reply that runs no js block ENDS THE TURN: whatever you wrote is your
answer to the user. So do not write a word until you have run what you
meant to run. Never announce what you are about to do ("I'll verify…",
"Next, let me…") — either do it in a js block in that same reply, or
say what you found. Announcing a step and running nothing wastes the
turn, and you will be asked again.

When the answer needs a clear boundary — there is machinery above it
you do not want read as the answer — put it in a stop block:

` + "```stop" + `
What you did, what you found, what is left.
` + "```" + `

Only what is inside it reaches the user then. The block is optional;
plain prose ends the turn just as well.

Either way the ending is final: never ask a question in it ("shall I
also…?", "do you want me to…?") — the turn is over by the time it is
read, so ask with tools.ask inside a js block, or decide and say what
you decided. And never end on a failed block: if your last block
errored, whatever you would claim is unverified.

Before you stop, run the task's own checks (its tests, a build, the
command the brief names) and fix what they show; stop only when the work
is actually done or you have hit a wall, and say which.

Answering:
- Your reply is read in a terminal. Be brief and direct: answer the
  question that was asked, in as few lines as it takes. No preamble
  ("Great question", "Let me explain"), no postamble summarising what
  you just did, no restating the request back.
- Give detail when the question calls for it — a design question, an
  explanation the user asked for, a report you were asked to write —
  and not otherwise. Match the length to the question, not to the
  effort you spent.
- Point at code as file/path.go:120 so the user can jump to it.
- Say what you actually found. If a check failed, show the failure; if
  you skipped something, say so; if you are unsure, say that rather
  than picking the answer the user seems to want. Being right matters
  more than being agreeable — disagree when you have reason to.
- Never invent a URL, a file path, an API, or a command's output.
- No emoji unless the user uses them first.

Conventions and safety:
- Read before you write: look at the surrounding file and its
  neighbours, and match the style, naming and idiom you find there.
- Never assume a dependency is available. Check the manifest (go.mod,
  package.json, Cargo.toml, pyproject.toml) or an existing import first.
- Do not add comments that restate the code, and do not reformat or
  "improve" lines the task did not ask you to touch.
- The working tree is the user's and may already be dirty. Never revert
  or stash a change you did not make, and never run a destructive git
  command (reset --hard, checkout --, clean -fd, push --force) unless
  the user asked for exactly that.
- Do not commit or push unless you were asked to.
- Ask only when you are genuinely blocked: do everything that does not
  depend on the answer first, then ask ONE question with the option you
  recommend. Never ask for permission to proceed.`

// TaskGuidance is the benchmark harness's extra brief, appended only
// when the loop row sets {task_guidance: true}. It is written for a
// graded task with hidden checks — find a defect in every module, keep
// every public interface — which is the opposite of what a person at a
// terminal wants when they ask what a codebase does. Bench only.
const TaskGuidance = `Be thorough over broad briefs: re-read the brief before finishing and
check each requirement against a change you made. When it says every
module or utility has defects, find a concrete defect in each one and
fix it against the textbook definition — a comment in the code that
names a shortcut ("biased", "approximate", "TODO") is a defect, not a
design choice. Make that concrete: early on, grep the code you are
fixing for TODO|FIXME|XXX|HACK|biased|approx|simplif|naive|placeholder|
for now, print the hits, and carry that list to the end — each hit is a
defect to fix (or to rule out with a reason in your final reply). Your
final reply lists each defect as file: what was wrong → what you
changed; never claim a fix you did not make.

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
const askPromptSection = `tools.ask(question, ...options) -> string blocks the turn until the user answers, so ask only what you cannot work out yourself. Never inline the options into the question text — pass each as its own argument, or they render as prose instead of clickable rows.`

// jobWake opens the turn a finished background job starts on its own.
const jobWake = "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\n"

var jsBlock = regexp.MustCompile("(?s)```js\\s*\n(.*?)```")

// anyBlock matches every fenced block; group 1 is the info string.
var anyBlock = regexp.MustCompile("(?s)```([^\\s`]*)[^\n]*\n.*?```")

const removedBlock = "[guessed output omitted]"

// fakeSystem matches a <system-…> pseudo-tag the MODEL wrote: a
// fabricated system message, paired with its closing tag or dangling.
// Real system text in bough comes from bough, never from a reply, so
// the whole span is the model's invention.
var fakeSystem = regexp.MustCompile(`(?is)<\s*system-[a-z0-9_-]*\s*>.*?(?:<\s*/\s*system-[a-z0-9_-]*\s*>|$)`)

// looseSystemTag catches a stray opening or closing tag left behind
// (mismatched names, a closing tag with no opening).
var looseSystemTag = regexp.MustCompile(`(?is)<\s*/?\s*system-[a-z0-9_-]*\s*>`)

const removedSystem = "[fabricated system message removed]"

// errEmptyReply is the turn error after two empty replies in a row.
var errEmptyReply = errors.New("the model returned an empty reply twice (provider hiccup) — send again, or switch with /model")

// outputTags are the fence info strings a model uses for invented
// results. A bare fence counts too. Anything else (```python,
// ```yaml, ```diff …) is code the model wants the user to see and
// stays.
var outputTags = map[string]bool{"": true, "output": true, "text": true, "txt": true,
	"plaintext": true, "console": true, "stdout": true, "stderr": true, "result": true, "log": true}

// invented reports whether a fence's info string is a model's word for
// runtime output. The exact list above misses the ones a model coins on
// the spot ("bg-output", "tool_result", "shell-stdout"), which then
// render as if bough had produced them.
func invented(tag string) bool {
	t := strings.ToLower(tag)
	if outputTags[t] {
		return true
	}
	for _, mark := range []string{"output", "stdout", "stderr", "result", "console"} {
		if strings.Contains(t, mark) {
			return true
		}
	}
	return false
}

// stripFakeBlocks replaces every output-looking fenced block
// (```output, ```text, bare ```...) in an assistant reply with
// removedBlock: those are model-guessed results, and left in the
// transcript they render like real runtime output. Language-tagged
// fences are kept: they are code being shown, not results.
// stripFakeSystem removes system messages the model invented. Seen in
// the wild from z-ai/glm-5.3-flash: a <system-variant-warmup> block
// announcing itself as an "AUTOMATED TEST MESSAGE", asserting a false
// repository state and instructing the agent to delete files and
// force-push. Left in place it renders as if bough had said it, and —
// worse — goes back into the model's own context as an authoritative
// prior instruction. Same treatment as an invented ```output fence:
// replaced by a marker, so the user still sees that it happened.
func stripFakeSystem(reply string) string {
	out := fakeSystem.ReplaceAllString(reply, removedSystem)
	return looseSystemTag.ReplaceAllString(out, removedSystem)
}

// extraBlocks is the marker replacing the code blocks after the first.
const extraBlocks = "[%d further code block(s) dropped — only the first block of a reply runs]"

// firstBlockOnly keeps a reply's first js block and replaces the rest
// with a marker, returning how many were dropped.
//
// A reply is a PLAN plus its first action; the actions after it were
// written blind, before their predecessor's output existed. Running
// them all made a degenerate reply catastrophic: one glm-5.3-flash
// reply carried 138 fenced blocks — a whole imagined session, complete
// with invented outputs between them — and the loop dutifully executed
// every one, so a single step produced 138 commands and 138 results
// the model then had to reconcile. One block per step also keeps the
// recorded reply small: the dropped text never re-enters the context.
func firstBlockOnly(reply string) (string, int) {
	locs := jsBlock.FindAllStringIndex(reply, -1)
	if len(locs) <= 1 {
		return reply, 0
	}
	// Everything after the first block goes, prose included: that prose
	// narrates results that do not exist yet ("The subagent has
	// finished. Verification confirms."), which is exactly the
	// invented-output problem in a different costume.
	return reply[:locs[0][1]] + "\n" + fmt.Sprintf(extraBlocks, len(locs)-1), len(locs) - 1
}

// FirstBlockOnly is firstBlockOnly for other plugins (workers runs the
// same one-block-per-step rule for subagents).
func FirstBlockOnly(reply string) (string, int) { return firstBlockOnly(reply) }

// StopAnswer is stopAnswer for other plugins: workers ends a child's
// run on the same contract.
func StopAnswer(reply string) (string, bool) { return stopAnswer(reply) }

// Finish applies the whole end-of-reply rule in one place: a js block
// means the reply is still working (run it, drop the rest), and a reply
// that runs nothing is the answer — with or without a stop fence.
// workers calls this rather than reimplementing it — a child that ran
// 74 blocks out of one hallucinated reply got there because the two
// copies had drifted apart.
//
// Running nothing IS stopping. Every other harness works this way: the
// Cline SDK completes "after the model returns text without tool
// calls", and Claude Code's Stop hook fires once the model has already
// decided to stop, as a veto. bough required the marker instead, so
// the model's most natural ending — a plain-prose final answer — was
// an error on every clean turn, and the answer arrived twice: once as
// the rejected draft, once reworded inside a fence. The fence is still
// honoured (it says where the answer starts); it is no longer the only
// way to hand the turn back. The reasons a stop is REFUSED live in the
// loop, where the turn's history is.
func Finish(reply string) (text string, stopped bool, dropped int) {
	if !jsFirst(reply) {
		if answer, ok := stopAnswer(reply); ok {
			return answer, true, 0
		}
		if jsBlock.FindStringIndex(reply) == nil {
			// stopAnswer declined, so any fence here is empty: keep the
			// prose around it and drop the marker, or the user reads
			// "```stop" as the answer. Nothing left means nothing was
			// said, which the loop refuses.
			return strings.TrimSpace(stopFence.ReplaceAllString(reply, "$1")), true, 0
		}
	}
	text, dropped = firstBlockOnly(reply)
	// A stop block under the block that is about to run is an answer
	// to output that does not exist yet. It cannot be honoured this
	// round, and left in the record it reads as the model's verdict.
	if loc := stopFence.FindStringIndex(text); loc != nil {
		text = strings.TrimRight(text[:loc[0]], "\n") + "\n" + fmt.Sprintf(extraBlocks, 1)
		dropped++
	}
	return text, false, dropped
}

// StripFabrications is stripFakeSystem for other plugins: a subagent's
// report crosses into the parent's context as tool output, so a child
// that invents a system message must not be able to hand it upward.
func StripFabrications(text string) string { return stripFakeSystem(text) }

func stripFakeBlocks(reply string) string {
	reply = stripFakeSystem(reply)
	return anyBlock.ReplaceAllStringFunc(reply, func(m string) string {
		tag := anyBlock.FindStringSubmatch(m)[1]
		if tag == "js" || tag == "stop" || !invented(tag) {
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
// jobNotePrefix opens the note a background job's "job" entry
// projects to.
const jobNotePrefix = "[background job] "

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
		case "nudge":
			// The loop's own push-back on a turn that stopped mid-plan.
			msgs = append(msgs, llm.Message{Role: "user", Content: text})
		case "job":
			// A background job finished (or matched its watch) while
			// the model was working: its notice is a user-side fact,
			// exactly like tool output.
			msgs = append(msgs, llm.Message{Role: "user", Content: jobNotePrefix + text})
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
	// stopRetries is how many times a turn is asked again for a stop
	// block. 0 (a bare runner in a test) keeps the old contract: any
	// block-less reply ends the turn.
	stopRetries int
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
	notices  Notices
	cat      func() string // the live tool catalogue; nil without the seam
	// schema, when set, is the shape this turn's stop block must
	// carry: the answer is validated and handed back on a mismatch.
	schema schema.Schema
	// shown is the injection summary already announced, so an
	// unchanged set is not re-announced every turn.
	shown string
	// snapSystem is the base prompt as the last turn assembled it,
	// under its own mutex: /context reads it from the ui goroutine
	// while a turn holds mu for its whole run.
	snapMu     sync.Mutex
	snapSystem string
	guidance   string // the benchmark brief, when task_guidance is set
	cp         Checkpointer
	secs       *Sections
	hasAsk     bool // an "ask-answers" service is mounted: document tools.ask
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
		if in, ok := res["input"].(string); ok && in != input {
			// A hook rewriting what you typed is not allowed to do it
			// behind your back.
			emit("context", "hook user-prompt-submit rewrote your message\n"+in)
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
			// Say which skill matched and show what it added: a skill
			// fires on a word in your message, so the model can be
			// following instructions you never saw.
			emit("context", "skill injected: "+skillName(block)+" ("+size(block)+")\n"+block)
		}
	}
	data := map[string]any{"text": msg.String()}
	// What the user actually typed, when the message sent is not it:
	// @file expansions and injected skills belong in the model's
	// context, not in the composer's Up-arrow history.
	if msg.String() != input {
		data["typed"] = input
	}
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

// skillName reads the name out of a "[skill: <name>]" block header.
func skillName(block string) string {
	line, _, _ := strings.Cut(block, "\n")
	if n, ok := strings.CutPrefix(strings.TrimSpace(line), "[skill:"); ok {
		return strings.TrimSpace(strings.TrimSuffix(n, "]"))
	}
	return "unknown"
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

// part is one contributor to the system prompt, for the context
// summary and /context.
type part struct {
	name string
	text string
}

// systemParts is the assembled prompt broken down by where each piece
// came from, in the order they are concatenated.
func (r *runner) systemParts(base string) []part {
	parts := []part{{"environment", envSection()}}
	if r.sysctx != nil {
		if cp, ok := r.sysctx.(contextParter); ok {
			for _, p := range cp.Parts() {
				name := filepath.Base(p.Path) + " (" + p.Path + ")"
				if p.Dropped > 0 {
					name += fmt.Sprintf(" — %d section(s) already in %s, dropped", p.Dropped, filepath.Base(p.Same))
				}
				parts = append(parts, part{name, p.Text})
			}
		} else if l, ok := r.sysctx.(loadedLister); ok {
			for _, p := range l.Loaded() {
				if body, err := os.ReadFile(p); err == nil {
					parts = append(parts, part{filepath.Base(p) + " (" + p + ")", string(body)})
				}
			}
		} else if pre := r.sysctx.Preamble(); pre != "" {
			parts = append(parts, part{"context files", pre})
		}
	}
	parts = append(parts, part{"base prompt", base})
	for _, name := range r.secs.Names() {
		parts = append(parts, part{"section " + name, r.secs.Get(name)})
	}
	return parts
}

// announceContext records what is being injected into the model's
// system prompt — the AGENTS.md/CLAUDE.md files, the plugin sections,
// the tool catalogue — the first time and whenever the set changes.
// Injection is otherwise invisible: the user sees a reply shaped by
// text they never saw.
func (r *runner) announceContext(emit func(kind, text string)) {
	r.snapMu.Lock()
	r.snapSystem = r.system
	r.snapMu.Unlock()
	parts := r.systemParts(r.system)
	var names []string
	total := 0
	var b strings.Builder
	for _, p := range parts {
		names = append(names, fmt.Sprintf("%s:%d", p.name, len(p.text)))
		total += len(p.text)
		fmt.Fprintf(&b, "- %s: %s\n", p.name, size(p.text))
	}
	sum := strings.Join(names, "|")
	if sum == r.shown || len(parts) <= 2 {
		return // nothing beyond the environment and the base prompt
	}
	r.shown = sum
	head := fmt.Sprintf("context: %d pieces, %d chars in the system prompt (/context to read it)",
		len(parts), total)
	// Event only, never a history entry: the injected text is already
	// in the record (a skill's block is part of the input entry, the
	// files are on disk) and an extra entry would shift the turn
	// numbering /tree and /undo show. A resumed session re-announces
	// on its first turn.
	emit("context", head+"\n"+strings.TrimRight(b.String(), "\n"))
}

// size renders a chunk's weight the way a reader thinks about it.
func size(s string) string {
	lines := strings.Count(strings.TrimRight(s, "\n"), "\n") + 1
	if s == "" {
		lines = 0
	}
	return fmt.Sprintf("%d lines, %d chars", lines, len(s))
}

// Context is the whole assembled system prompt with a header naming
// each piece — what /context prints. Built fresh when no turn has run
// yet, so it is readable before you say anything.
func (r *runner) Context() string {
	if r.cat != nil {
		if c := r.cat(); c != "" {
			r.secs.Set("tools", "Available in the runtime:\n"+c+
				"\n- console.log(...): print; everything printed is returned to you")
		}
	}
	r.snapMu.Lock()
	base := r.snapSystem
	r.snapMu.Unlock()
	if base == "" {
		base = SystemPrompt // no turn yet: what the next one will use
	}
	parts := r.systemParts(base)

	var b strings.Builder
	b.WriteString("Everything the model is told before your message:\n\n")
	for _, p := range parts {
		fmt.Fprintf(&b, "## %s — %s\n\n%s\n\n", p.name, size(p.text), strings.TrimRight(p.text, "\n"))
	}
	return strings.TrimRight(b.String(), "\n")
}

// landJobs records every notice a background job has queued as a "job"
// entry, so the next model step sees it. Cheap when none are pending,
// which is the normal case.
func (r *runner) landJobs(emit func(kind, text string)) {
	if r.notices == nil {
		return
	}
	for _, text := range r.notices.Take() {
		r.note(emit, "job", text, nil)
	}
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
	// A reasoning model's thinking is streamed to the ui as it arrives
	// and recorded once at the end. It is NEVER fed back: DefaultProject
	// ignores "thinking" entries, so the model re-reasons each step
	// instead of reading its own half-thoughts as fact.
	if th, ok := r.llm.(llm.ThinkingStreamer); ok {
		var think strings.Builder
		reply, err := th.StreamThinking(ctx, sys, msgs,
			func(delta string) { emit("assistant-delta", delta) },
			func(delta string) {
				think.WriteString(delta)
				emit("thinking-delta", delta)
			})
		if t := strings.TrimSpace(think.String()); t != "" {
			r.hist.Append("thinking", map[string]any{"text": t})
			emit("thinking", t)
		}
		return reply, err
	}
	if st, ok := r.llm.(llm.Streamer); ok {
		return st.Stream(ctx, sys, msgs, func(delta string) { emit("assistant-delta", delta) })
	}
	return r.llm.Complete(ctx, sys, msgs)
}

// stopFence matches the block that ends a turn: everything the model
// wants the user to read goes inside it. The closing fence is
// optional — a model that opens ```stop and simply writes to the end
// of its reply has stopped, and nothing after it would run anyway.
// Requiring it cost three calls and a confusing "did not stop" note
// per turn on glm-5.3-flash, which omits it about half the time.
var stopFence = regexp.MustCompile("(?s)```stop[^\n]*\n(.*?)(?:```|$)")

// stoppedOnErrorNote is fed back when the model stops immediately
// after a failed block. Cline's attempt_completion has the same rule —
// never complete before confirming the previous tool use succeeded —
// and it is the difference between "done" and "done, apparently".
const stoppedOnErrorNote = "[unfinished] Your last block FAILED and you stopped on it: the error above is the last thing that happened, so whatever you just claimed is unverified. Fix it, or run the check again, or stop with an honest account of what failed and what you did not do."

// schemaSection tells the model the stop block must be JSON of a
// given shape. Appended per turn, never baked into the base prompt: a
// schema is set by the caller (headless --schema, a spawn), not by the
// session.
const SchemaSection = `This turn's answer is STRUCTURED. The stop block must contain JSON matching this schema and nothing else — no prose above it, no fence inside it, no trailing commentary:

%s

Everything you want to say goes in the JSON's own fields. Get it right the first time: an answer that does not match is handed back to you with the mismatches.`

// schemaNote is fed back when the stop block does not match the
// schema. The issues are the model's own mistakes in its own terms —
// the recovery path a constrained decoder gives you for free, done
// here by checking and asking again.
const SchemaNote = "[unfinished] Your stop block does not match the schema this turn requires:\n\n%s\n\nAnswer again with a stop block containing ONLY the corrected JSON."

// askedInStopNote is fed back when the final answer ends in a question.
const askedInStopNote = "[unfinished] You ended the turn with a question. The turn is over when you stop, so the user reads it with no way to answer inside it. Either decide it yourself and say what you decided, or ask properly with tools.ask(question, ...options) inside a js block, which blocks until they answer."

// announcedNote is fed back to a model that described its next action
// instead of taking it. This is the one refusal the old
// stop-block-or-retry contract was really earning: of 22 nudges in a
// week of real sessions, 7 recovered work, and every one of those was
// a reply that announced a step it had not run. The other 15 were
// finished answers that came back reworded.
const announcedNote = "[unfinished] You said what you were about to do instead of doing it, and ran nothing — so nothing happened. Do it now in a ```js block. If it turns out there is nothing left to run, just say what you found; a reply that runs nothing ends the turn."

// saidNothingNote is fed back for a reply with no content: no code, no
// words.
const saidNothingNote = "[unfinished] That reply said nothing: no ```js block, so nothing ran, and no words, so the user has no answer. Do the next thing in a ```js block, or answer them."

// tagRe strips HTML-ish tags so saysNothing can see what is left.
var tagRe = regexp.MustCompile(`(?s)<[^>]*>`)

// saysNothing reports whether a reply carries no content at all — once
// markup is removed, not a single letter or digit remains.
//
// This generalises the empty-reply check, and it is the shape the
// specific vetoes kept missing one at a time: a turn ended on "<br>",
// an earlier one on "}". Both are what a model emits when it has lost
// the thread, and neither is an answer. A short reply is still fine —
// "Done.", "3 lines." — because those say something.
func saysNothing(reply string) bool {
	plain := tagRe.ReplaceAllString(reply, "")
	for _, r := range plain {
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			return false
		}
	}
	return true
}

// announceRe matches prose that promises the model's own next action
// ("Let me check…", "Now running…", "I'll verify…"). It is deliberately
// verb-anchored: a bare "I'll" also appears in finished answers ("I'll
// be notified when it finishes"), which must not be refused.
var announceRe = regexp.MustCompile(`(?i)\b(?:` +
	`(?:let me|let'?s|i'?ll|i will|i'?m going to|now i'?ll|next,? i'?ll)\s+(?:\w+\s+){0,2}` +
	`(?:check|run|read|look|write|fix|verify|test|make|add|see|confirm|inspect|probe|dig|gather|start|build|try|patch|apply|update|remove|search|find|open|dump|measure|port|wire|land)` +
	`|(?:now|first|next),?\s+(?:checking|running|reading|writing|fixing|verifying|testing|looking|probing|gathering|building)` +
	`|running it now|doing that now` +
	`)\b`)

// truncatedNote is fed back when the provider cut a reply off at the
// output limit. The partial text stays in history — the model wrote it
// and the next call needs it — but it cannot be the turn's answer.
const truncatedNote = "[unfinished] Your reply was cut off at the output limit, so it stops mid-thought and is not an answer. Say it again, shorter: lead with the conclusion, drop the recap, and if there is genuinely more to do, do it in a ```js block instead of describing it."

// meantToRunNote is fed back when a reply tried to call a tool in a
// form the loop cannot run.
const meantToRunNote = "[unfinished] You wrote a tool call, but not inside a ```js block, so nothing ran and nobody saw it. Only a fenced ```js block is executed — no <script> tags, no other language tags, no bare code. Write the block again, properly fenced."

// scriptTag and wrongFence are the two shapes a misfenced tool call
// arrives in: HTML wrapping (a real reply opened with <html><body>…
// <script>console.log(tools.write(…))</script>) and a fence tagged
// with the wrong language.
var (
	scriptTag = regexp.MustCompile(`(?i)<script[\s>]`)
	// jsonCall: a reply that is nothing but a JSON object. Models fall
	// back to the tool-calling convention they were trained on and
	// answer with {"cmd": "find …"} — no prose around it, so it is not
	// an answer to anybody, and no tools.* call, so the other shapes
	// miss it.
	jsonCall   = regexp.MustCompile(`(?s)\A\s*\{.*\}\s*\z`)
	wrongFence = regexp.MustCompile("(?s)```(?:javascript|typescript|ts|node|jsx)[^\n]*\n(.*?)```")
	toolLine   = regexp.MustCompile(`(?m)^\s*(?:console\.log\(\s*)?tools\.\w+\(`)
)

// meantToRunCode reports whether a reply attempted a tool call the loop
// cannot execute. Under the old contract this was covered by accident —
// anything without a stop block was pushed back — and losing it cost a
// real turn: a model answered with <html><body><script>console.log(
// tools.write(…))</script></body></html>, nothing ran, and because the
// reply held no js block the turn ENDED with that markup as the answer.
//
// The check runs only when no js block was found, so a properly fenced
// reply never reaches it. It errs toward pushing back: a false veto
// costs one round-trip and the model answers again, while a miss hands
// the user a non-answer and burns the turn.
func meantToRunCode(reply string) bool {
	if jsBlock.FindStringIndex(reply) != nil {
		return false
	}
	if scriptTag.MatchString(reply) {
		return true
	}
	if m := wrongFence.FindStringSubmatch(reply); m != nil && strings.Contains(m[1], "tools.") {
		return true
	}
	if jsonCall.MatchString(strings.TrimSpace(reply)) {
		return true
	}
	// A bare tool call at the start of a line: prose that mentions
	// tools.bash inline is discussion, a line that begins with one is
	// an attempt to run it.
	return toolLine.MatchString(reply)
}

// announcesWork reports whether a reply promises an action rather than
// reporting one. A reply that trails off into a colon counts too: the
// block it was introducing never arrived.
func announcesWork(reply string) bool {
	t := strings.TrimSpace(reply)
	if t == "" {
		return false
	}
	return announceRe.MatchString(t) || strings.HasSuffix(t, ":")
}

// stopAnswer returns the turn's final answer when the reply carries a
// stop block: the prose around it plus the block's body, with the
// fence markers gone. ok is false when there is no stop block, or when
// it and the prose around it are empty (nothing to end a turn with).
func stopAnswer(reply string) (string, bool) {
	loc := stopFence.FindStringSubmatchIndex(reply)
	if loc == nil {
		return "", false
	}
	body := strings.TrimSpace(reply[loc[2]:loc[3]])
	// Prose before the fence is kept (a model likes to introduce its
	// answer); anything after it is dropped, like a second js block.
	// Any FENCE in that prose goes too: an answer is words, and a
	// reply that imagines a whole session before stopping must not
	// smuggle 74 code blocks into it.
	head := strings.TrimSpace(jsBlock.ReplaceAllString(reply[:loc[0]], ""))
	out := strings.TrimSpace(head + "\n\n" + body)
	return out, out != ""
}

// jsFirst reports whether a js block comes before the reply's stop
// block: a model that plans a step and then stops in the same reply
// gets its step run, and is asked again afterwards.
func jsFirst(reply string) bool {
	js := jsBlock.FindStringIndex(reply)
	stop := stopFence.FindStringIndex(reply)
	return js != nil && (stop == nil || js[0] < stop[0])
}

// defaultStopRetries is how many times a turn is asked again for a
// stop block before the loop gives up and takes the reply as final: a
// model that cannot follow the contract must not cost an unbounded
// number of calls, and the user must never be left with nothing.
const defaultStopRetries = 2

// outOfSteps// outOfSteps is the last thing the model is asked when the step budget
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

	// The catalogue is rebuilt per turn: a plugin mounted mid-session
	// (an mcp server, a reloaded init.js tool) documents itself here.
	if r.cat != nil {
		if c := r.cat(); c != "" {
			r.secs.Set("tools", "Available in the runtime:\n"+c+
				"\n- console.log(...): print; everything printed is returned to you")
		}
	}
	if !r.started {
		r.started = true
		r.system = SystemPrompt
		if r.guidance != "" {
			r.system += "\n\n" + r.guidance
		}
		if r.hasAsk {
			r.system += "\n\n" + askPromptSection
		}
		if res := r.fire(ctx, "session-start", map[string]any{}, emit); res != nil {
			if c, ok := res["context"].(string); ok && c != "" {
				r.system += "\n\n" + c
				emit("context", "hook session-start added context ("+size(c)+")\n"+c)
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
	if len(r.schema) > 0 {
		system += "\n\n" + fmt.Sprintf(SchemaSection, r.schema.Describe())
	}
	r.announceContext(emit)

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
	nudges := 0         // push-backs spent asking for a stop block
	lastFailed := false // the previous block errored: a stop on it is unverified
	for step := 0; step < maxSteps; step++ {
		r.landSteers(ctx, emit, false) // a steer sent during the last block joins the context now
		r.landJobs(emit)               // a background job that finished during the last block reports now
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
			text := err.Error()
			// Outgrowing the window is a dead end rather than a
			// failure: say what to do instead of leaving the user with
			// a provider's token arithmetic.
			if llm.IsOverflow(err) {
				text += llm.OverflowHelp
			}
			note("error", text, nil)
			finish("", r.doneData()) // every turn ends with a done, even on llm failure
			return err
		}
		retried = false
		reply = stripFakeBlocks(reply)
		// A stop block ends the turn: its body (plus any prose before
		// it) IS the answer, and nothing after it runs.
		stopped := false
		if answer, ok := stopAnswer(reply); ok && !jsFirst(reply) {
			reply, stopped = answer, true
		}
		dropped := 0
		if !stopped {
			reply, dropped = firstBlockOnly(reply)
		}
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
			// A reply that runs nothing has ended the turn (Finish).
			// What is left here is the veto: the handful of reasons a
			// turn may NOT end yet, which is how a stop hook works
			// everywhere else — the model decides it is done, and the
			// harness overrules it with a reason when it is wrong.
			// A stop is refused when the reply only announces work, on
			// the heels of a failed block (the claim is unverified),
			// when it asks the user something (nobody can answer a
			// finished turn), and when a schema is unmet.
			why, note0 := "", ""
			switch {
			case saysNothing(reply):
				why, note0 = "said nothing", saidNothingNote
			case strings.Contains(reply, llm.Truncated):
				why, note0 = "was cut off at the output limit", truncatedNote
			// Not under a schema: a structured turn's answer IS a bare
			// JSON object, which is the shape jsonCall refuses. The
			// schema case below is what judges those.
			case len(r.schema) == 0 && meantToRunCode(reply):
				why, note0 = "wrote a tool call that was not in a ```js block", meantToRunNote
			case announcesWork(reply):
				why, note0 = "announced work it did not do", announcedNote
			case lastFailed:
				why, note0 = "stopped straight after a failed block", stoppedOnErrorNote
			case len(r.schema) > 0:
				// A structured turn ends on a valid answer or not at
				// all: the mismatches go back in the model's own terms.
				if _, issues := r.schema.ValidateJSON(reply); len(issues) > 0 {
					why = "does not match the schema"
					note0 = fmt.Sprintf(SchemaNote, "- "+strings.Join(issues, "\n- "))
				}
			case strings.HasSuffix(strings.TrimSpace(reply), "?"):
				why, note0 = "ended the turn with a question", askedInStopNote
			}
			if why != "" {
				if nudges < r.stopRetries {
					nudges++
					// The point of refusing a stop-on-failure is to
					// make the model look again, not to trap it: once
					// it has been told, an honest report gets through.
					lastFailed = false
					r.hist.Append("nudge", map[string]any{"text": note0})
					// The reply stays in history (the model said it,
					// and the next call needs it), but on screen it is
					// superseded by the answer that follows: the user
					// should not read the same thing twice.
					note("system", fmt.Sprintf("that reply %s; asking again (%d/%d)", why, nudges, r.stopRetries),
						map[string]any{"supersedes": true})
					continue
				}
				if r.stopRetries > 0 {
					note("system", "still "+why+" after "+strconv.Itoa(r.stopRetries)+" tries; taking the reply as final", nil)
				}
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
			out = capOutput(noneNoted(out), maxResultBytes)
			if dropped > 0 {
				out += fmt.Sprintf("\n\n[only the first of your %d code blocks ran. Write ONE block per reply, read its output, then decide the next one.]", dropped+1)
			}
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
				lastFailed = true
				emit("error", out)
			} else {
				lastFailed = false
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
	// Keep the head AND the tail. A tail-only cut threw away three of
	// six subagent reports and left the third mid-sentence; a report's
	// conclusion lives at its end, so both ends are worth more than the
	// middle. Never cut inside a multi-byte rune.
	head, tail := n*2/3, n-n*2/3
	for head > 0 && !utf8.RuneStart(s[head]) {
		head--
	}
	for tail > 0 && !utf8.RuneStart(s[len(s)-tail]) {
		tail--
	}
	return fmt.Sprintf("%s\n… [%d bytes cut] …\n%s", s[:head], len(s)-head-tail, s[len(s)-tail:])
}

// spillDirOverride points the spill directory somewhere else (tests);
// empty means ~/.bough/spill.
var spillDirOverride string

// writeSpill saves s to a new file under ~/.bough/spill and returns
// its path. CreateTemp, not a numbered name: two loops in one process
// (workers) must not race for one slot.
func writeSpill(s string) (string, error) {
	dir := spillDirOverride
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		dir = filepath.Join(home, ".bough", "spill")
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	f, err := os.CreateTemp(dir, "result-*.log")
	if err != nil {
		return "", err
	}
	if _, err := f.WriteString(s); err != nil {
		f.Close()
		return "", err
	}
	if err := f.Close(); err != nil {
		return "", err
	}
	return f.Name(), nil
}

// capOutput cuts a block result down to n bytes the way truncate
// does, but saves the whole output first and appends the spill file's
// path and line count, so the hidden middle is a grep away instead of
// gone. When the spill write fails it degrades to the bare cut.
func capOutput(s string, n int) string {
	if len(s) <= n {
		return s
	}
	if path, err := writeSpill(s); err == nil {
		lines := strings.Count(strings.TrimRight(s, "\n"), "\n") + 1
		return truncate(s, n) +
			fmt.Sprintf("\n[full output saved to %s — %d lines; use tools.view or grep it]", path, lines)
	}
	return truncate(s, n)
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
	if c, ok := code.(cataloguer); ok {
		r.cat = c.Catalogue
	}
	if v, ok := cfg["task_guidance"]; ok {
		on, ok := v.(bool)
		if !ok {
			if s, isStr := v.(string); isStr {
				on, ok = s == "true", s == "true" || s == "false"
			}
		}
		if !ok {
			return fmt.Errorf("loop: task_guidance must be a bool, got %v", v)
		}
		if on {
			r.guidance = TaskGuidance
		}
	}
	r.stopRetries = defaultStopRetries
	if v, ok := cfg["stop_retries"]; ok {
		n, err := toInt(v)
		if err != nil || n < 0 {
			return fmt.Errorf("loop: stop_retries must be a non-negative integer, got %v", v)
		}
		r.stopRetries = n
	}
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
	if n, err := kernel.Get[Notices](kctx, "job-notices"); err == nil {
		r.notices = n
	}
	// A structured turn: the launcher's --schema, so a scripted run
	// gets JSON it can pipe instead of prose it has to parse.
	if sc, err := kernel.Get[schema.Schema](kctx, "stop-schema"); err == nil {
		r.schema = sc
	}
	if c, err := kernel.Get[Checkpointer](kctx, "checkpoints"); err == nil {
		r.cp = c
	}
	kctx.Provide("runner", r)
	// /context prints what the model is told before your message: the
	// AGENTS.md files, the plugin sections, the generated tool
	// catalogue. Optional seam — headless has no command registry.
	if reg, err := kernel.Get[*commands.Registry](kctx, "commands"); err == nil {
		info := commands.CommandInfo{Name: "context", Summary: "show everything injected into the system prompt"}
		if err := reg.Register(info, func(string) (string, error) { return r.Context(), nil }); err != nil {
			return err
		}
		kctx.Effect(func() { reg.Unregister("context") })
	}

	inputs := make(chan string, 8)
	kctx.Provide("inputs", inputs)

	t := &turns{}
	kctx.Provide("cancel", t.Cancel)
	r.steer = t.takeSteers
	kctx.Provide("steer", t.Steer)

	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		emit := func(kind, text string) {
			// Live gets what replay gets: a noted entry's data
			// (the done marker's files and exit) rides along.
			kctx.Emit("loop/event", Event{Kind: kind, Text: text, Data: r.noteData})
		}
		// A background job that finishes while the agent is idle has
		// nobody to tell: its wake starts a turn of its own. A job that
		// finishes mid-turn is landed by landJobs instead, so the wake
		// then finds nothing pending and starts nothing.
		var wake <-chan struct{}
		if r.notices != nil {
			wake = r.notices.Wake()
		}
		for {
			select {
			case input, ok := <-inputs:
				if !ok {
					return
				}
				t.run(ctx, r, input, emit)
			case <-wake:
				pending := r.notices.Take()
				if len(pending) == 0 {
					continue
				}
				news := strings.Join(pending, "\n\n")
				// The turn nobody asked for still says where it came
				// from: without this row the transcript shows a reply
				// with no question above it.
				emit("job", news)
				t.run(ctx, r, jobWake+news, emit)
			case <-ctx.Done():
				return
			}
		}
	}()
	kctx.Effect(func() {
		cancel()
		close(inputs)
	})
	return nil
}
