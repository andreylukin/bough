//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"pgregory.net/rapid"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/plugins/history"
)

// reactive is a model that answers the newest thing it was sent, so
// any interleaving the property draws has a well-defined reply: a user
// line's marker picks a call, a failure or a slow reply; anything else
// (a tool result, a notice) gets a short text.
type reactive struct {
	requests atomic.Int64
	calls    atomic.Int64
}

func (m *reactive) AgentAdapter(agentllm.Options) (agentllm.Adapter, error) { return m, nil }
func (m *reactive) Provider() string                                        { return "fake" }
func (m *reactive) Model() string                                           { return "reactive" }
func (m *reactive) Close() error                                            { return nil }

func (m *reactive) Respond(ctx context.Context, r ullm.Request, _ ullm.RequestOptions) (ullm.Response, error) {
	m.requests.Add(1)
	text := ""
	if n := len(r.Input); n > 0 {
		if msg, ok := r.Input[n-1].Data.(ullm.Message); ok && msg.Role == ullm.RoleUser {
			text = msg.Text
		}
	}
	id := func(p string) string { return fmt.Sprintf("%s%d", p, m.calls.Add(1)) }
	reply := func(items ...ullm.Item) (ullm.Response, error) {
		return ullm.Response{ID: fmt.Sprintf("r%d", m.requests.Load()), Stop: ullm.StopComplete, Output: items}, nil
	}
	switch {
	case strings.Contains(text, "FAIL"):
		return ullm.Response{}, errBoom
	case strings.Contains(text, "HOLD"):
		return reply(ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: id("h"), Name: "hold", Arguments: `{"text":"h"}`}})
	case strings.Contains(text, "ECHO"):
		return reply(ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: id("e"), Name: "echo", Arguments: `{"text":"e"}`}})
	case strings.Contains(text, "SLOW"):
		select {
		case <-time.After(40 * time.Millisecond):
		case <-ctx.Done():
			return ullm.Response{}, ctx.Err()
		}
	}
	return reply(ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: "ok"}})
}

// holdKit is echo plus a hold whose calls finish one per release.
type holdKit struct {
	reg agenttools.Registry
	rel chan struct{}
	all chan struct{}
}

func newHoldKit(t *rapid.T) *holdKit {
	k := &holdKit{reg: agenttools.NewRegistry(), rel: make(chan struct{}, 64), all: make(chan struct{})}
	obj := agenttools.Object(nil, map[string]any{"text": agenttools.Prop("string", "text")})
	detail := func(json.RawMessage) string { return "x" }
	for _, tl := range []agenttools.Tool{
		{Name: "echo", Description: "echo", Schema: obj, Detail: detail,
			Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
				return agenttools.Result{Text: "echoed"}, nil
			}},
		{Name: "hold", Description: "hold", Schema: obj, Detail: detail,
			Call: func(ctx context.Context, _ agenttools.Call) (agenttools.Result, error) {
				select {
				case <-k.rel:
				case <-k.all:
				case <-ctx.Done():
					return agenttools.Result{}, ctx.Err()
				}
				return agenttools.Result{Text: "released"}, nil
			}},
	} {
		if _, err := k.reg.Register(tl); err != nil {
			t.Fatal(err)
		}
	}
	return k
}

// §9.11: whatever the interleaving of input, steer, cancel, notices,
// call completions, provider errors, supersedes and settle expiry,
// every submitted line gets exactly one done, every wake one done, a
// steer and a mid-turn notice none, cancelled is always followed at
// once by done, and nothing reaches the provider after a cancel until
// something new is said.
func TestDoneAccountingProperty(t *testing.T) {
	t.Parallel()
	// The bubble's fake clock runs the settle, SLOW, wait and
	// after-cancel sleeps (~4s a run in real time) instantly, and the
	// after-cancel check sees every goroutine parked instead of racing
	// a 50ms window. What remains is the harness store's fsync on every
	// append, ~3ms each on macOS (F_FULLFSYNC, serialised per device):
	// ~5s here for 100 runs, well under 1s where fsync is cheap. rapid
	// gets the T wrapped because it calls T.Deadline, which panics in a
	// bubble.
	synctest.Test(t, func(t *testing.T) {
		rapid.Check(struct{ testing.TB }{t}, func(rt *rapid.T) {
			dir := t.TempDir()
			h, err := history.Open(filepath.Join(dir, "history", "p.jsonl"))
			if err != nil {
				rt.Fatal(err)
			}
			defer h.Close()
			model := &reactive{}
			kit := newHoldKit(rt)
			jobs := &fakeJobs{wake: make(chan struct{}, 1)}
			short := rapid.Bool().Draw(rt, "short_settle")
			settleFor := time.Minute
			if short {
				settleFor = 30 * time.Millisecond
			}
			r, err := Open(context.Background(), Deps{
				SessionID: "p",
				Store:     filepath.Join(dir, "engine"),
				Scratch:   func() string { return filepath.Join(dir, "scratch") },
				Cwd:       dir,
				Config: Config{TurnSettle: settleFor, CallTimeout: time.Minute,
					SteerInterrupts: rapid.Bool().Draw(rt, "steer_interrupts")},
				LLM:     func() (agentllm.Source, string, error) { return model, "fake", nil },
				Tools:   kit.reg,
				Jobs:    func() Jobs { return jobs },
				History: h,
			})
			if err != nil {
				rt.Fatal(err)
			}
			defer func() {
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				defer cancel()
				_ = r.Close(ctx)
			}()

			submits := 0
			// A steer Steer accepted can still find its turn closed by the
			// time the actor takes it; it then runs as an input of its own.
			accepted := map[string]bool{}
			ops := rapid.SliceOfN(rapid.SampledFrom([]string{
				"text", "hold", "echo", "fail", "slow", "steer", "cancel", "release", "notice", "wait",
			}), 1, 10).Draw(rt, "ops")
			for i, op := range ops {
				line := fmt.Sprintf("line%d", i)
				switch op {
				case "text", "hold", "echo", "fail", "slow":
					r.Submit(line + " " + strings.ToUpper(op))
					submits++
				case "steer":
					// What ui does: a refused steer is sent as input.
					if !r.Steer(line + " steer") {
						r.Submit(line + " steer")
						submits++
					} else {
						accepted[line+" steer"] = true
					}
				case "cancel":
					r.Cancel()
					if !short {
						// With nothing adopted, only something new said after
						// the cancel — a line queued behind the cancelled
						// turn, a notice — may call the model; each of those
						// records its input first. A request already on its
						// way when the cancel landed gets 50ms to show up.
						time.Sleep(50 * time.Millisecond)
						before := model.requests.Load()
						if saidNothingSince(h.Entries()) {
							time.Sleep(100 * time.Millisecond)
							if after := model.requests.Load(); after != before && saidNothingSince(h.Entries()) {
								rt.Fatalf("%d provider requests after a cancel with nothing new said\n%s", after-before, dumpEntries(h.Entries()))
							}
						}
					}
				case "release":
					kit.rel <- struct{}{}
				case "notice":
					jobs.notify("news " + line)
				case "wait":
					time.Sleep(time.Duration(rapid.IntRange(1, 60).Draw(rt, "ms")) * time.Millisecond)
				}
			}
			close(kit.all)
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			defer cancel()
			// A notice reaches the actor through the watch goroutine, so a
			// Drain can run before it. Once the actor has taken every notice
			// (Take runs on the actor), a Drain queued after that sees the
			// turn it opened.
			for {
				jobs.mu.Lock()
				left := len(jobs.news)
				jobs.mu.Unlock()
				if left == 0 {
					break
				}
				time.Sleep(10 * time.Millisecond)
			}
			if err := r.Drain(ctx); err != nil {
				rt.Fatalf("drain: %v\n%s", err, dumpEntries(h.Entries()))
			}

			es := h.Entries()
			pending, wakes, inputs := 0, 0, 0
			for i, e := range es {
				switch e.Kind {
				case "input":
					switch {
					case e.Data["steer"] == true:
					case e.Data["wake"] == true:
						wakes++
					default:
						pending++
						if !accepted[fmt.Sprint(e.Data["text"])] {
							inputs++
						}
					}
				case "done":
					if e.Data["wake"] == true {
						wakes--
					} else {
						pending--
					}
					if pending < 0 || wakes < 0 {
						rt.Fatalf("a done with no turn open at entry %d\n%s", e.Seq, dumpEntries(es))
					}
				case "cancelled":
					if i+1 >= len(es) || es[i+1].Kind != "done" {
						rt.Fatalf("cancelled at entry %d is not followed by done\n%s", e.Seq, dumpEntries(es))
					}
				}
			}
			if pending != 0 || wakes != 0 {
				rt.Fatalf("%d turns and %d wakes never got their done\n%s", pending, wakes, dumpEntries(es))
			}
			if inputs != submits {
				rt.Fatalf("%d lines submitted, %d recorded as turns\n%s", submits, inputs, dumpEntries(es))
			}
		})
	})
}

// saidNothingSince reports whether the last cancel is newer than every
// input: nothing has been said to the model since.
func saidNothingSince(es []history.Entry) bool {
	lastCancel, lastInput := -1, -1
	for i, e := range es {
		switch e.Kind {
		case "cancelled":
			lastCancel = i
		case "input":
			lastInput = i
		}
	}
	return lastCancel > lastInput
}

var dumpMu sync.Mutex

func dumpEntries(es []history.Entry) string {
	dumpMu.Lock()
	defer dumpMu.Unlock()
	var b strings.Builder
	for _, e := range es {
		d := map[string]any{}
		for k, v := range e.Data {
			if k != "checkpoint" && k != "system_file" && k != "store" && k != "system" {
				d[k] = v
			}
		}
		j, _ := json.Marshal(d)
		fmt.Fprintf(&b, "%3d %-10s %s\n", e.Seq, e.Kind, j)
	}
	return b.String()
}
