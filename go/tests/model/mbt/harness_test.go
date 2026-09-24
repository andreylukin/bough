//go:build !windows

// Package mbt runs FizzBee model-based tests against a real `bough
// serve`: the spec under go/tests/model/specs is model-checked, the
// fizzbee-mbt server walks its state graph, and an adapter per spec
// drives the server through internal/servetest and llm-control and
// reports the abstract state it observes. The recipe for adding a flow
// is go/tests/model/README.md; example_test.go is the worked example.
//
// The fizz tools come from scripts/fizz.sh (pinned, sha256-checked). A
// test skips unless FIZZ_BIN, FIZZBEE_MBT_SERVER and FIZZBEE_MBT_BIN
// name them, because fetching them is network and go test is offline;
// scripts/model-test.sh exports all three.
package mbt

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

func TestMain(m *testing.M) {
	// The lib's flags (--max-seq-runs, --seq-seed, ...) override a
	// test's options from the command line when reproducing a failure.
	fmbt.ParseFlags()
	servetest.Main(m)
}

// fizzTools returns the pinned fizz, fizzbee-mbt-server and runner, or
// skips: they are fetched by scripts/fizz.sh, which is network.
func fizzTools(t *testing.T) (fizz, server string) {
	t.Helper()
	fizz, server = os.Getenv("FIZZ_BIN"), os.Getenv("FIZZBEE_MBT_SERVER")
	if fizz == "" || server == "" || os.Getenv("FIZZBEE_MBT_BIN") == "" {
		t.Skip("FIZZ_BIN, FIZZBEE_MBT_SERVER and FIZZBEE_MBT_BIN unset; run scripts/model-test.sh")
	}
	return fizz, server
}

// specPath is go/tests/model/specs/<name>.fizz.
func specPath(name string) string {
	_, file, _, _ := runtime.Caller(0)
	return filepath.Join(filepath.Dir(file), "..", "specs", name+".fizz")
}

// fizzCheck model-checks specs/<name>.fizz and returns the run dir with
// its state graph. The spec is copied into a temp dir first: fizz writes
// the parsed AST next to the spec, and parallel tests would race on it.
// fizz exits 0 on a failed check, so pass is read off its output.
func fizzCheck(t *testing.T, name string) string {
	t.Helper()
	fizz, _ := fizzTools(t)
	src, err := os.ReadFile(specPath(name))
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	spec := filepath.Join(dir, name+".fizz")
	if err := os.WriteFile(spec, src, 0o644); err != nil {
		t.Fatal(err)
	}
	run := filepath.Join(dir, "run")
	out, err := exec.Command(fizz, "--output-dir", run, spec).CombinedOutput()
	if err != nil || !bytes.Contains(out, []byte("\nPASSED:")) {
		t.Fatalf("fizz check %s: %v\n%s", name, err, out)
	}
	return run
}

// mbtPort is where fizzbee-mbt-runner 0.2.0 dials the state-graph
// server; it has no flag for another address (go/tests/model/GRAPH.md).
const mbtPort = 50051

// lockMBT serialises MBT runs across every process on the machine: the
// runner's server port is fixed, so two runs at once would walk each
// other's graphs. The flock is released when the test ends, or when
// the process dies.
func lockMBT(t *testing.T) {
	t.Helper()
	f, err := os.OpenFile(filepath.Join(os.TempDir(), "bough-fizz-mbt-50051.lock"), os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		f.Close()
		t.Fatal(err)
	}
	t.Cleanup(func() {
		syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
		f.Close()
	})
}

// startGraphServer serves runDir's graph on mbtPort until the test ends.
// Call it with the lock held.
func startGraphServer(t *testing.T, runDir string) {
	t.Helper()
	_, server := fizzTools(t)
	addr := fmt.Sprintf("127.0.0.1:%d", mbtPort)
	if c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond); err == nil {
		c.Close()
		t.Fatalf("port %d is already taken by something that is not holding the MBT lock", mbtPort)
	}
	var out bytes.Buffer
	cmd := exec.Command(server, "--port", fmt.Sprint(mbtPort), "--states_file", runDir+"/")
	cmd.Stdout, cmd.Stderr = &out, &out
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	exited := make(chan struct{})
	go func() { cmd.Wait(); close(exited) }()
	t.Cleanup(func() {
		cmd.Process.Kill()
		<-exited
		if t.Failed() {
			t.Logf("fizzbee-mbt-server output:\n%s", out.String())
		}
	})
	deadline := time.Now().Add(10 * time.Second)
	for {
		if c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond); err == nil {
			c.Close()
			return
		}
		select {
		case <-exited:
			t.Fatalf("fizzbee-mbt-server exited: %s", out.String())
		default:
		}
		if time.Now().After(deadline) {
			t.Fatalf("fizzbee-mbt-server not listening after 10s: %s", out.String())
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// runMBT model-checks the spec, serves its graph and lets the runner
// walk it against model. It returns the runner's error: a state the
// adapter reports that the graph does not allow, an action that failed,
// or the runner itself failing.
func runMBT(t *testing.T, spec string, model fmbt.Model, actions map[string]map[string]fmbt.ActionFunc, opts map[string]any) error {
	t.Helper()
	// The runners share one graph server, so they queue behind each
	// other, and at 100-8000 random runs apiece the package took eleven
	// minutes. The deterministic walks (the *Paths tests) reach every
	// state on every run in seconds; the random runs add breadth, and
	// they run where time is not the point: the exhaustive
	// MODEL_COVER=transitions job.
	if envCover() != tracecheck.CoverTransitions {
		t.Skip("fizzbee-mbt random runs are part of the exhaustive run: MODEL_COVER=transitions")
	}
	runDir := fizzCheck(t, spec)
	lockMBT(t)
	startGraphServer(t, runDir)
	return fmbt.RunTests(t, model, actions, opts)
}

// sessionHistory reads a serve session's transcript off disk: the
// history file is the record the trace check replays, not the API's
// view of it.
func sessionHistory(t *testing.T, home, id string) []history.Entry {
	t.Helper()
	entries, err := history.Read(filepath.Join(home, ".bough", "history", id+".jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	return entries
}

// checkHistory replays the abstract trace project derives from a
// session's history against the spec's graph.
func checkHistory(t *testing.T, g *tracecheck.Graph, entries []history.Entry, project func([]history.Entry) []tracecheck.Step) {
	t.Helper()
	steps := project(entries)
	if v := g.Check(steps); v != nil {
		b, _ := json.Marshal(steps)
		t.Errorf("history trace is not a path in the model: %v\ntrace: %s", v, b)
	}
}

// TestSpecFixtures keeps testdata/<spec>/ honest: the browser specs walk
// the paths generated from that checked-in graph, so it must be the
// graph specs/<spec>.fizz has now. `scripts/model-test.sh gen <spec>`
// rewrites it.
func TestSpecFixtures(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	specs, _ := filepath.Glob(filepath.Join(filepath.Dir(specPath("x")), "*.fizz"))
	if len(specs) == 0 {
		t.Fatal("no specs under go/tests/model/specs")
	}
	for _, p := range specs {
		name := strings.TrimSuffix(filepath.Base(p), ".fizz")
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			want, err := tracecheck.Load(filepath.Join(filepath.Dir(p), "..", "testdata", name))
			if err != nil {
				t.Fatalf("%v (run: scripts/model-test.sh gen %s)", err, name)
			}
			got, err := tracecheck.Load(fizzCheck(t, name))
			if err != nil {
				t.Fatal(err)
			}
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("testdata/%s is not the graph of specs/%s.fizz; run: scripts/model-test.sh gen %s", name, name, name)
			}
		})
	}
}

// historyProjections maps a spec name to how its trace is read off a
// session's history. TestHistoryTraces uses it for the transcripts the
// Playwright model specs leave behind; each flow registers its own.
var historyProjections = map[string]func([]history.Entry) []tracecheck.Step{}

// TestHistoryTraces checks transcripts the browser model specs saved
// under $MODEL_TRACE_DIR/<spec>/*.jsonl against specs/<spec>.fizz.
func TestHistoryTraces(t *testing.T) {
	t.Parallel()
	root := os.Getenv("MODEL_TRACE_DIR")
	if root == "" {
		t.Skip("MODEL_TRACE_DIR unset; scripts/model-test.sh sets it after the Playwright model specs")
	}
	dirs, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	checked := 0
	for _, d := range dirs {
		if !d.IsDir() {
			continue
		}
		spec := d.Name()
		project, ok := historyProjections[spec]
		if !ok {
			t.Errorf("%s: no history projection registered for spec %q", root, spec)
			continue
		}
		g, err := tracecheck.Load(fizzCheck(t, spec))
		if err != nil {
			t.Fatal(err)
		}
		files, _ := filepath.Glob(filepath.Join(root, spec, "*.jsonl"))
		for _, f := range files {
			entries, err := history.Read(f)
			if err != nil {
				t.Fatal(err)
			}
			t.Run(spec+"/"+strings.TrimSuffix(filepath.Base(f), ".jsonl"), func(t *testing.T) {
				checkHistory(t, g, entries, project)
			})
			checked++
		}
	}
	if checked == 0 {
		t.Fatalf("no transcripts under %s", root)
	}
}

// controlConfig routes the llm row to llm-control, so each turn answers
// as the adapter queued it and can be held in "running".
const controlConfig = "- id: llm\n  plugin: llm-control\n"

// gate is how an adapter honours the spec's `require`s. The runner (0.2.0)
// picks actions at random, disabled ones included, runs the whole walk,
// and only then validates it, stopping silently at the first action the
// graph does not enable: nothing after it is checked, whatever state the
// adapter reports. So a disabled action is a no-op, and so is every
// action after it, which keeps a walk from spending real turns on steps
// nobody checks.
type gate struct{ off bool }

// pass says whether to act: enabled is the action's `require`, read off
// the adapter's own view of the state.
func (g *gate) pass(enabled bool) bool {
	if !enabled {
		g.off = true
	}
	return !g.off
}

// reset opens the gate for a new walk; call it from Init.
func (g *gate) reset() { g.off = false }

// action adapts a method with no choice arguments to fmbt.ActionFunc.
func action[M any](f func(M) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) { return nil, f(m.(M)) }
}

// actionTimeout bounds one step against the serve: a turn is released
// by hand, so anything slower is a hang.
const actionTimeout = 30 * time.Second

func actionCtx() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), actionTimeout)
}

// waitRow polls the session until ok holds. It returns an error rather
// than failing the test: inside an action the runner reports the step.
func waitRow(s *servetest.Server, id, what string, ok func(serve.Row) bool) (serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.WaitSession(ctx, id, ok)
	if err != nil {
		return row, fmt.Errorf("waiting for %s: %w", what, err)
	}
	return row, nil
}

// pathsJSON is the walks over testdata/<spec>'s checked-in graph, in the
// shape the generator's paths.json had ({"paths":[{"trace":[...]}]}), so
// each flow's decoder reads it unchanged. The paths are derived at test
// time rather than checked in: the per-target files were 745k lines, and a
// walk replaces dozens of per-target paths that each rebooted serve.
// MODEL_COVER=transitions takes every link instead of reaching every
// settled state; that exhaustive run is the nightly one.
func pathsJSON(spec string) ([]byte, error) { return pathsJSONCover(spec, envCover()) }

// pathsJSONCover is pathsJSON with the cover fixed, for a test whose point
// needs every link (a wrong adapter caught on one particular transition).
func pathsJSONCover(spec string, cover tracecheck.Cover) ([]byte, error) {
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath(spec)), "..", "testdata", spec))
	if err != nil {
		return nil, err
	}
	return json.Marshal(struct {
		Paths []tracecheck.Walk `json:"paths"`
	}{g.Walks(cover, 0)})
}

// envCover is the run's cover: every settled state unless
// MODEL_COVER=transitions asks for every link.
func envCover() tracecheck.Cover {
	if os.Getenv("MODEL_COVER") == string(tracecheck.CoverTransitions) {
		return tracecheck.CoverTransitions
	}
	return tracecheck.CoverStates
}
