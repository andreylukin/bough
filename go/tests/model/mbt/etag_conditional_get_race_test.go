//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"sort"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/etag_conditional_get_race.fizz: one poller's conditional GET
// against writeJSONTagged (internal/serve/etag.go), racing a write to
// the content it serves.
//
// The trick that lets this run against a real serve with no timing
// games: writeJSONTagged decides fresh-vs-304 for good the instant it
// answers, so StartPoll makes that real, whole HTTP round trip right
// away and only holds on to the answer; Respond is the adapter
// revealing what already happened. A Write the adapter runs between
// the two still lands strictly after the real read (Go's single test
// goroutine cannot run both at once), which is exactly the model's
// gap: the answer StartPoll captured can already be stale by the time
// Respond looks at it.
type etagAdapter struct {
	t   *testing.T
	s   *servetest.Server
	dir string // this walk's session cwd, so its own poll sees only it
	id  string
	gate

	version   int
	inflight  bool
	snapshot  int
	clientTag int
	result    string

	lastETag string // the real ETag a "fresh" answer carried, for the next If-None-Match
	pendStat int    // the real status StartPoll's GET already got, held for Respond
	pendETag string

	// wrongAdapter is the deliberate bug
	// TestEtagConditionalGetRaceCatchesWrongAdapter injects: Respond
	// treats every poll as fresh, even one the server actually answered
	// 304.
	wrongAdapter bool
}

func newEtagAdapter(t *testing.T) *etagAdapter {
	s := servetest.Start(t, servetest.Options{})
	return &etagAdapter{t: t, s: s, dir: s.Dir(t, "work")}
}

// Init starts each walk on a fresh idle session in the same serve, in
// its own cwd: /api/sessions?cwd=... then serves only this walk's row,
// so an earlier walk's session sitting in the same serve cannot change
// the content this one polls.
func (a *etagAdapter) Init() error {
	a.gate.reset()
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.dir, "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.version, a.inflight, a.snapshot, a.clientTag, a.result = 0, false, -1, -1, ""
	a.lastETag, a.pendStat, a.pendETag = "", 0, ""
	return nil
}

func (a *etagAdapter) Cleanup() error { return nil }

func (a *etagAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Poll", Index: 0}: a}, nil
}

func (a *etagAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"version":    a.version,
		"inflight":   a.inflight,
		"snapshot":   a.snapshot,
		"client_tag": a.clientTag,
		"result":     a.result,
	}, nil
}

// poll is the real conditional GET: /api/sessions, scoped to this
// walk's own cwd, carrying the last ETag this walk was handed. Both the
// status and the ETag come straight off the response writeJSONTagged
// sent; nothing here reimplements its decision.
func (a *etagAdapter) poll() (status int, etag string, err error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet,
		a.s.URL+"/api/sessions?all=1&cwd="+url.QueryEscape(a.dir), nil)
	if err != nil {
		return 0, "", err
	}
	req.Header.Set("Authorization", "Bearer "+a.s.Token)
	if a.lastETag != "" {
		req.Header.Set("If-None-Match", a.lastETag)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	return resp.StatusCode, resp.Header.Get("ETag"), nil
}

// StartPoll is the handler's read of the current content and its
// decision against If-None-Match: a real request, answered for good
// right here. snapshot is the version that decision was made against.
func (a *etagAdapter) StartPoll() error {
	if !a.gate.pass(!a.inflight) {
		return nil
	}
	a.inflight = true
	a.snapshot = a.version
	status, etag, err := a.poll()
	if err != nil {
		return err
	}
	a.pendStat, a.pendETag = status, etag
	return nil
}

// Write is a turn landing a new message: a real change to the content,
// through a plain title rename (cheap, and distinct every time so the
// content's hash always moves).
func (a *etagAdapter) Write() error {
	if !a.gate.pass(a.version < 3) {
		return nil
	}
	a.version++
	ctx, cancel := actionCtx()
	defer cancel()
	return a.s.Rename(ctx, a.id, fmt.Sprintf("v%d", a.version))
}

// Respond is the adapter looking at the answer StartPoll's real request
// already got. A Write that landed since does not change it: that is
// the race.
func (a *etagAdapter) Respond() error {
	if !a.gate.pass(a.inflight) {
		return nil
	}
	fresh := a.pendStat != http.StatusNotModified
	if a.wrongAdapter {
		fresh = true
	}
	if fresh {
		a.result = "fresh"
		a.clientTag = a.snapshot
		a.lastETag = a.pendETag
	} else {
		a.result = "not_modified"
	}
	a.inflight = false
	return nil
}

func etagAction(name string, f func(*etagAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) { return nil, f(m.(*etagAdapter)) }
}

var etagActions = map[string]map[string]fmbt.ActionFunc{"Poll": {
	"StartPoll": etagAction("StartPoll", (*etagAdapter).StartPoll),
	"Write":     etagAction("Write", (*etagAdapter).Write),
	"Respond":   etagAction("Respond", (*etagAdapter).Respond),
}}

// Every step is a real HTTP round trip against a real serve, so random
// walks are cheap enough for the default runs too; they run only in
// the exhaustive job (runMBT skips otherwise).
func etagOptions() map[string]any {
	return map[string]any{"max-seq-runs": 150, "max-actions": 6, "max-parallel-runs": 0}
}

// etagDiff compares the adapter's state with what the spec predicted
// for one step.
func etagDiff(want, got map[string]any) string {
	var d []string
	for k, v := range want {
		f, ok := strings.CutPrefix(k, "Poll#0.")
		if !ok {
			continue
		}
		if fmt.Sprint(got[f]) != fmt.Sprint(v) {
			d = append(d, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
		}
	}
	sort.Strings(d)
	if d == nil {
		return ""
	}
	return strings.Join(d, "; ") + fmt.Sprintf(" (state %v)", got)
}

// walkEtagPaths walks every generated path for cover and compares the
// adapter's state with the spec's at each step. Every path runs, so one
// run reports every divergence.
func walkEtagPaths(a *etagAdapter, cover tracecheck.Cover) error {
	b, err := pathsJSONCover("etag_conditional_get_race", cover)
	if err != nil {
		return err
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		return err
	}
	var errs []error
	for i, p := range doc.Paths {
		if err := a.walk(p.Trace); err != nil {
			var names []string
			for _, s := range p.Trace {
				names = append(names, strings.TrimPrefix(s.Action, "Poll#0."))
			}
			errs = append(errs, fmt.Errorf("path %d %v: %w", i, names, err))
		}
	}
	return errors.Join(errs...)
}

func (a *etagAdapter) walk(trace []tracecheck.Step) error {
	if err := a.Init(); err != nil {
		return fmt.Errorf("step 0 (Init): %w", err)
	}
	for j, s := range trace[1:] {
		name := strings.TrimPrefix(s.Action, "Poll#0.")
		if _, err := etagActions["Poll"][name](a, nil); err != nil {
			return fmt.Errorf("step %d (%s): %w", j+1, s.Action, err)
		}
		if a.gate.off {
			return fmt.Errorf("step %d (%s): the adapter found it disabled", j+1, s.Action)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w", j+1, s.Action, err)
		}
		if d := etagDiff(s.State, got); d != "" {
			return fmt.Errorf("step %d (%s): %s", j+1, s.Action, d)
		}
	}
	return nil
}

// TestEtagConditionalGetRace lets fizzbee-mbt walk the spec at random.
func TestEtagConditionalGetRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newEtagAdapter(t)
	if err := runMBT(t, "etag_conditional_get_race", a, etagActions, etagOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// TestEtagConditionalGetRacePaths walks every generated path against a
// real serve.
func TestEtagConditionalGetRacePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newEtagAdapter(t)
	if err := walkEtagPaths(a, envCover()); err != nil {
		t.Fatalf("spec path: %v", err)
	}
}

// The run above proves nothing unless an adapter that reports the wrong
// thing fails it: this one calls every poll "fresh", including the
// polls the server actually answered 304.
func TestEtagConditionalGetRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newEtagAdapter(t)
	a.wrongAdapter = true
	if err := walkEtagPaths(a, tracecheck.CoverStates); err == nil {
		t.Fatal("a run whose Respond calls every poll fresh passed; the paths are not checking state")
	}
}
