package servetest_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serveclient"
	"github.com/andreylukin/bough/internal/servetest"
)

func TestMain(m *testing.M) { servetest.Main(m) }

// One server end to end over the verbs the control room uses: a session
// is created with a prompt, the echo reply lands in its transcript and
// on its event stream, a second prompt runs another turn, and archive
// hides it from the default list.
func TestServeRoundTrip(t *testing.T) {
	t.Parallel()
	s := servetest.Start(t, servetest.Options{})
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	cwd := s.Dir(t, "work")
	row, err := s.CreateSession(ctx, cwd, "say FIRST! please")
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	stream, err := s.Events(ctx, row.ID)
	if err != nil {
		t.Fatalf("events: %v", err)
	}
	defer stream.Close()

	done, err := s.WaitSession(ctx, row.ID, func(r serve.Row) bool { return r.Status == serve.StatusDone })
	if err != nil {
		t.Fatalf("first turn: %v\n%s", err, s.Output())
	}
	if done.Cwd != cwd {
		t.Errorf("cwd = %q, want %q", done.Cwd, cwd)
	}
	if _, err := stream.WaitFor(ctx, func(e serve.Event) bool { return e.Kind == "done" }); err != nil {
		t.Fatalf("event stream never carried done: %v", err)
	}

	if err := s.Prompt(ctx, row.ID, "say SECOND! please"); err != nil {
		t.Fatalf("prompt: %v", err)
	}
	// The row still reads "done" from the first turn until the prompt
	// lands, so wait on the second input's entries having grown past it.
	if _, err := s.WaitSession(ctx, row.ID, func(r serve.Row) bool { return r.Status == serve.StatusDone && r.Entries > done.Entries+1 }); err != nil {
		t.Fatalf("second turn: %v\n%s", err, s.Output())
	}
	_, lines, err := s.GetSession(ctx, row.ID)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	var text strings.Builder
	for _, l := range lines {
		text.WriteString(l.Text + "\n")
	}
	for _, want := range []string{"FIRST!", "SECOND!"} {
		if !strings.Contains(text.String(), want) {
			t.Errorf("transcript lacks %q:\n%s", want, text.String())
		}
	}

	if _, err := s.Stop(ctx, row.ID); err != nil {
		t.Errorf("stop: %v", err)
	}
	if _, err := s.Archive(ctx, row.ID); err != nil {
		t.Fatalf("archive: %v", err)
	}
	rows, err := s.ListSessions(ctx, false)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(rows) != 0 {
		t.Errorf("archived session still listed: %+v", rows)
	}
	all, err := s.ListSessions(ctx, true)
	if err != nil || len(all) != 1 || !all[0].Archived {
		t.Errorf("list all = %+v, %v; want the one archived session", all, err)
	}
}

// A restart keeps HOME, address and token: the session is still there,
// nothing of the old serve's process group outlives it, the same binary
// answers as the same build, and a copy of it as another one.
func TestServeRestarts(t *testing.T) {
	t.Parallel()
	s := servetest.Start(t, servetest.Options{})
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "say HI! please")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.WaitSession(ctx, row.ID, func(r serve.Row) bool { return r.Status == serve.StatusDone && r.Live }); err != nil {
		t.Fatal(err)
	}
	before, err := s.Build(ctx)
	if err != nil || before == "" {
		t.Fatalf("build = %q, %v", before, err)
	}
	s.Shutdown()
	if s.GroupAlive() {
		t.Fatal("a process of the stopped serve's group is still running")
	}
	if _, err := s.Build(ctx); err == nil {
		t.Fatal("a stopped serve answered")
	}
	if err := s.Resume(""); err != nil {
		t.Fatal(err)
	}
	got, _, err := s.GetSession(ctx, row.ID)
	if err != nil || got.Status != serve.StatusDone || got.Live {
		t.Fatalf("after restart: %+v, %v; want the done session with no child", got, err)
	}
	if b, _ := s.Build(ctx); b != before {
		t.Errorf("same binary restarted as build %q, was %q", b, before)
	}
	s.Shutdown()
	copyBin := filepath.Join(s.Root, "bough-new")
	if err := os.Link(s.Bin(), copyBin); err != nil {
		t.Fatal(err)
	}
	if err := s.Resume(copyBin); err != nil {
		t.Fatal(err)
	}
	if b, _ := s.Build(ctx); b == before || b == "" {
		t.Errorf("a new binary answered as build %q, the old one was %q", b, before)
	}
}

// An API error comes back typed with its status, not as a decode
// failure: a test asserting a refusal needs the code.
func TestServeErrorsCarryStatus(t *testing.T) {
	t.Parallel()
	s := servetest.Start(t, servetest.Options{})
	_, _, err := s.GetSession(context.Background(), "no-such-session")
	var apiErr *servetest.APIError
	if !errors.As(err, &apiErr) || apiErr.Status != 404 {
		t.Fatalf("err = %v, want a 404 APIError", err)
	}
}

// Eight servers up at the same moment, each on its own HOME, port and
// token, each running a turn concurrently with the others: every one
// sees only its own session, and its reply echoes its own prompt. All
// eight start before any turn runs, so they are alive together rather
// than taking turns under -parallel.
func TestEightServersSideBySide(t *testing.T) {
	t.Parallel()
	const n = 8
	servers := make([]*servetest.Server, n)
	addrs := map[string]bool{}
	for i := range servers {
		servers[i] = servetest.Start(t, servetest.Options{})
		addrs[servers[i].Addr] = true
	}
	if len(addrs) != n {
		t.Fatalf("servers share an address: %v", addrs)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	var wg sync.WaitGroup
	errs := make([]error, n)
	for i, s := range servers {
		cwd := s.Dir(t, "work")
		wg.Go(func() { errs[i] = oneTurn(ctx, s, cwd, fmt.Sprintf("MARK%d!", i)) })
	}
	wg.Wait()
	for i, err := range errs {
		if err != nil {
			t.Errorf("server %d (%s): %v\n%s", i, servers[i].Addr, err, servers[i].Output())
		}
	}
	for i, s := range servers {
		if !strings.HasPrefix(s.Home, s.Root) {
			t.Errorf("server %d: home %s outside root %s", i, s.Home, s.Root)
		}
		if tok := serveclient.ReadToken(s.Home); tok == "" || tok != s.Token {
			t.Errorf("server %d: token %q is not the one in its HOME (%q)", i, s.Token, tok)
		}
	}
}

func oneTurn(ctx context.Context, s *servetest.Server, cwd, word string) error {
	row, err := s.CreateSession(ctx, cwd, "say "+word+" please")
	if err != nil {
		return fmt.Errorf("create: %w", err)
	}
	if _, err := s.WaitSession(ctx, row.ID, func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	rows, err := s.ListSessions(ctx, true)
	if err != nil || len(rows) != 1 || rows[0].ID != row.ID {
		return fmt.Errorf("list = %+v, %v; want only %s", rows, err, row.ID)
	}
	_, lines, err := s.GetSession(ctx, row.ID)
	if err != nil {
		return fmt.Errorf("get: %w", err)
	}
	for _, l := range lines {
		if l.Kind == "assistant" && strings.Contains(l.Text, "echo: say "+word) {
			return nil
		}
	}
	return fmt.Errorf("no assistant echo of %q in %+v", word, lines)
}
