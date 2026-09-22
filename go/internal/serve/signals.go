package serve

import (
	"maps"
	"regexp"
	"slices"
	"strconv"
	"time"

	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
)

// Signals a session carries besides its status, read from its history
// alone so a headless child needs no extra channel to report them.

// Job is a background job the session started and has not heard back
// from. A job's own process lives in the child, so a job whose child is
// gone was killed with it and is not listed.
type Job struct {
	ID      int       `json:"id"`
	Cmd     string    `json:"cmd"`
	Started time.Time `json:"started"`
}

// Legacy fallback: history files written before typed job entries only
// carry the model-facing text, so these regexes read it.
var (
	jobStarted  = regexp.MustCompile(`(?m)^job (\d+) started in the background \([^)]*\): (.*)$`)
	jobFinished = regexp.MustCompile(`^job (\d+) \[`)
)

// typedJob reports whether e is a typed job entry (tools writes "event";
// the loop's notice job entries carry only text).
func typedJob(e history.Entry) bool {
	_, ok := e.Data["event"].(string)
	return e.Kind == "job" && ok
}

// RunningJobs lists the jobs started and not yet finished, oldest first.
// Job ids restart with the child, so a later start of the same id
// replaces the earlier one.
func RunningJobs(entries []history.Entry, childAlive bool) []Job {
	if !childAlive {
		return nil
	}
	open := map[int]Job{}
	typed := false
	for _, e := range entries {
		if !typedJob(e) {
			continue
		}
		typed = true
		id := int(num(e.Data["id"]))
		switch e.Data["event"] {
		case "started":
			cmd, _ := e.Data["cmd"].(string)
			open[id] = Job{ID: id, Cmd: cmd, Started: e.At}
		case "finished":
			delete(open, id)
		}
	}
	for _, e := range entries {
		if typed {
			break
		}
		t, _ := e.Data["text"].(string)
		switch e.Kind {
		case "result":
			for _, m := range jobStarted.FindAllStringSubmatch(t, -1) {
				id, _ := strconv.Atoi(m[1])
				open[id] = Job{ID: id, Cmd: m[2], Started: e.At}
			}
		case "job":
			if m := jobFinished.FindStringSubmatch(t); m != nil {
				id, _ := strconv.Atoi(m[1])
				delete(open, id)
			}
		}
	}
	out := slices.Collect(maps.Values(open))
	slices.SortFunc(out, func(a, b Job) int { return a.Started.Compare(b.Started) })
	return out
}

// troubleWindow bounds how old an unseen failure can be and still ask for
// attention: without it every failure ever recorded floods Needs you the
// first time the feature is on. A week covers a weekend away.
const troubleWindow = 7 * 24 * time.Hour

// backgroundTroubleWindow is the same bound for a run nobody sat in front
// of. A background agent that was interrupted is its parent's to deal
// with, and a parent that has not by the next day is not going to; two
// six-day-old research agents headed Needs you every morning for a week.
const backgroundTroubleWindow = 24 * time.Hour

// hasInput reports whether anyone ever sent the session a message.
func hasInput(entries []history.Entry) bool {
	for _, e := range entries {
		if e.Kind == "input" {
			return true
		}
	}
	return false
}

// Troubled says why a session's outcome still needs a person, or "" when
// it does not: it failed, was interrupted (not stopped on purpose), or its
// last test run failed — within the window, and with something recorded
// after the last time it was marked seen. The reason is what the sidebar
// shows, so a finished session is not just "Done" in a queue of trouble.
func Troubled(st Status, entries []history.Entry, ack int64, now time.Time) string {
	if len(entries) == 0 {
		return ""
	}
	last := entries[len(entries)-1]
	if last.Seq <= ack || now.Sub(last.At) >= troubleWindow {
		return ""
	}
	switch {
	case st == StatusError:
		return "failed"
	case st == StatusInterrupted:
		return "interrupted"
	case lastTestFailed(entries):
		// A turn can finish cleanly while the tests it ran failed:
		// finishing is the lifecycle, the exit is the verdict.
		return "tests failed"
	}
	return ""
}

// testCmd matches a command that runs a test suite (the web view's
// Tests chip uses the same list).
var testCmd = regexp.MustCompile(`\b(go test|(?:npm|pnpm|yarn|bun)(?: run)? test|pytest|cargo (?:test|nextest)|vitest|jest|make (?:test|check)|mvn test|gradle test|rspec|phpunit)\b`)

// lastTestFailed reports whether the most recent test command recorded a
// non-zero exit on its result.
func lastTestFailed(entries []history.Entry) bool {
	failed, _ := lastTest(entries)
	return failed
}

// lastTest is the most recent test command with a recorded exit: whether
// it failed and when its result was written (zero when there is none).
// The web view ages test status from this, never from the last entry.
func lastTest(entries []history.Entry) (bool, time.Time) {
	for i := len(entries) - 1; i >= 0; i-- {
		if _, exit, ok := callTest(entries[i]); ok {
			return exit != 0, entries[i].At
		}
		if i == 0 {
			break
		}
		r, c := entries[i], entries[i-1]
		if r.Kind != "result" || c.Kind != "code" {
			continue
		}
		exit, ok := r.Data["exit"].(float64)
		if !ok {
			if n, isInt := r.Data["exit"].(int); isInt {
				exit, ok = float64(n), true
			}
		}
		code, _ := c.Data["text"].(string)
		if ok && testCmd.MatchString(code) {
			return exit != 0, r.At
		}
	}
	return false, time.Time{}
}

// Cache is the prompt cache as of the last turn that reported one: when
// that turn ended, what it read from and wrote to the cache, and how
// long the prefix stays warm after it. The UI counts down from At.
type Cache struct {
	At    time.Time `json:"at"`
	TTL   int       `json:"ttl"` // seconds
	Read  int       `json:"read"`
	Write int       `json:"write"`
	In    int       `json:"in"`
}

// LastCache is the cache state after the most recent turn whose provider
// reported cache tokens; nil when none ever did.
func LastCache(entries []history.Entry, model string) *Cache {
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		if e.Kind != "done" {
			continue
		}
		u, ok := e.Data["usage"].(map[string]any)
		if !ok {
			continue
		}
		c := Cache{At: e.At, TTL: int(usageTTL(u, model) / time.Second), Read: int(num(u["cache_read"])), Write: int(num(u["cache_write"])), In: int(num(u["in"]))}
		if c.Read+c.Write > 0 {
			return &c
		}
	}
	return nil
}

// usageTTL is how long the turn's cache writes stay warm. An engine turn
// records the TTL it actually asked for (usage.ttl: "1h", "5m", or
// seconds), which the model's default cannot know: the engine writes
// one-hour entries on models whose default is five minutes.
func usageTTL(u map[string]any, model string) time.Duration {
	switch v := u["ttl"].(type) {
	case string:
		if d, err := time.ParseDuration(v); err == nil && d > 0 {
			return d
		}
	case float64:
		if v > 0 {
			return time.Duration(v) * time.Second
		}
	case int:
		if v > 0 {
			return time.Duration(v) * time.Second
		}
	}
	return llm.CacheTTL(model)
}
