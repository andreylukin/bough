package serve

import (
	"maps"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/plugins/history"
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

var (
	jobStarted  = regexp.MustCompile(`(?m)^job (\d+) started in the background \([^)]*\): (.*)$`)
	jobFinished = regexp.MustCompile(`^job (\d+) \[`)
)

// RunningJobs lists the jobs started and not yet finished, oldest first.
// Job ids restart with the child, so a later start of the same id
// replaces the earlier one.
func RunningJobs(entries []history.Entry, childAlive bool) []Job {
	if !childAlive {
		return nil
	}
	open := map[int]Job{}
	for _, e := range entries {
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

// Troubled says whether a session's outcome still needs a person: it
// failed or was interrupted (not stopped on purpose), within the window,
// and recorded something after the last time it was marked seen.
func Troubled(st Status, entries []history.Entry, ack int64, now time.Time) bool {
	if (st != StatusError && st != StatusInterrupted) || len(entries) == 0 {
		return false
	}
	last := entries[len(entries)-1]
	return last.Seq > ack && now.Sub(last.At) < troubleWindow
}

// cacheTTLFor is how long a provider keeps a used prompt prefix, as its
// docs state it: Anthropic's default ephemeral cache is five minutes;
// OpenAI keeps prefixes for at least 30 minutes on GPT-5.6 and later
// (gpt-6 included) and five to ten on earlier models. Unknown providers
// get the short window, so the chip errs toward "cold".
func cacheTTLFor(model string) time.Duration {
	m := strings.ToLower(strings.TrimPrefix(model, "~"))
	if gpt := regexp.MustCompile(`gpt-(\d+)(?:\.(\d+))?`).FindStringSubmatch(m); gpt != nil {
		major, _ := strconv.Atoi(gpt[1])
		minor, _ := strconv.Atoi(gpt[2])
		if major > 5 || (major == 5 && minor >= 6) {
			return 30 * time.Minute
		}
	}
	return 5 * time.Minute
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
		c := Cache{At: e.At, TTL: int(cacheTTLFor(model) / time.Second), Read: int(num(u["cache_read"])), Write: int(num(u["cache_write"])), In: int(num(u["in"]))}
		if c.Read+c.Write > 0 {
			return &c
		}
	}
	return nil
}
