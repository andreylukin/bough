package serve

import (
	"maps"
	"regexp"
	"slices"
	"strconv"
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

// CacheTTL is how long a provider keeps a used prompt prefix: Anthropic's
// ephemeral cache and OpenAI's automatic one both hold about five minutes.
const CacheTTL = 5 * time.Minute

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
func LastCache(entries []history.Entry) *Cache {
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		if e.Kind != "done" {
			continue
		}
		u, ok := e.Data["usage"].(map[string]any)
		if !ok {
			continue
		}
		c := Cache{At: e.At, TTL: int(CacheTTL / time.Second), Read: int(num(u["cache_read"])), Write: int(num(u["cache_write"])), In: int(num(u["in"]))}
		if c.Read+c.Write > 0 {
			return &c
		}
	}
	return nil
}
