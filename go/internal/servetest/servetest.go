// Package servetest starts a real `bough serve --run` for a test and
// talks to it over the same HTTP API the control room uses.
//
// Each server gets its own temp HOME (with a bough.yml that routes the
// llm row to llm-echo unless the test says otherwise), its own loopback
// port and its own token, so any number can run side by side in one
// test process and none reaches the real ~/.bough or the network. The
// bough binary comes from testbin: BOUGH_BIN, or one cached build
// shared with every other suite of the run.
//
// Test-only: nothing outside _test.go files should import it.
package servetest

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serveclient"
	"github.com/andreylukin/bough/internal/testbin"
)

// DefaultConfig is the HOME overlay a server starts with: the embedded
// rows, with the model swapped for the deterministic echo provider.
const DefaultConfig = "- id: llm\n  plugin: llm-echo\n"

// Options shape one server. The zero value is an echo server.
type Options struct {
	// Config replaces DefaultConfig as $HOME/.bough/bough.yml.
	Config string
	// Files are written under HOME before start, path relative to HOME
	// (e.g. ".bough/init.js").
	Files map[string]string
	// Env is appended to the child's environment ("K=V").
	Env []string
	// Dir is where serve starts, relative to HOME and created before
	// start; "" is HOME. It is the folder first-run setup offers.
	Dir string
	// ReadyTimeout bounds the wait for /api/health; default 30s.
	ReadyTimeout time.Duration
}

// Server is one running `bough serve --run`.
type Server struct {
	URL   string // http://127.0.0.1:<port>
	Addr  string // 127.0.0.1:<port>
	Token string
	Root  string // the temp dir everything below lives in
	Home  string // $HOME for the server and its sessions

	// cmd and exited are the current process: Shutdown and Resume replace
	// them, so a test that restarts serve calls those from one goroutine.
	cmd    *exec.Cmd
	bin    string
	opts   Options
	out    *safeBuf
	exited chan struct{}
	close  sync.Once
	client *http.Client
}

// launch starts serve from bin on s's HOME and address.
func (s *Server) launch(bin string) error {
	cmd := exec.Command(bin, "serve", "--run", s.Addr)
	cmd.Dir = filepath.Join(s.Home, s.opts.Dir)
	if err := os.MkdirAll(cmd.Dir, 0o755); err != nil {
		return err
	}
	cmd.Env = childEnv(s.Home, s.opts.Env)
	cmd.Stdout, cmd.Stderr = s.out, s.out
	setProcessGroup(cmd)
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("servetest: start %s: %w", bin, err)
	}
	exited := make(chan struct{})
	go func() { cmd.Wait(); close(exited) }()
	s.cmd, s.bin, s.exited = cmd, bin, exited
	return nil
}

// Shutdown stops serve the way launchd or `bough update` does, with
// SIGTERM, and waits for it; its HOME, port and token stay for Resume.
// A serve still up after 5s is killed with its whole process group.
func (s *Server) Shutdown() {
	select {
	case <-s.exited:
		return
	default:
	}
	terminate(s.cmd)
	select {
	case <-s.exited:
	case <-time.After(5 * time.Second):
		killGroup(s.cmd)
		<-s.exited
	}
}

// Command is bin with args run as this server's processes run: on its
// HOME and environment (plus env), from its start directory, output into
// Output(), in a process group of its own. A test that plays a second
// process on the same HOME (a second serve, `bough serve stop`, a
// terminal `bough -r`) starts it with this and owns it: it starts,
// waits for and kills it itself.
func (s *Server) Command(bin string, env []string, args ...string) *exec.Cmd {
	cmd := exec.Command(bin, args...)
	cmd.Dir = filepath.Join(s.Home, s.opts.Dir)
	cmd.Env = append(childEnv(s.Home, s.opts.Env), env...)
	cmd.Stdout, cmd.Stderr = s.out, s.out
	setProcessGroup(cmd)
	return cmd
}

// Resume starts serve again on the same HOME and address after Shutdown,
// from bin ("" is the binary it last ran), and waits for it to answer.
// A restart test uses a copy of the binary as the "new build".
func (s *Server) Resume(bin string) error {
	if bin == "" {
		bin = s.bin
	}
	if err := s.launch(bin); err != nil {
		return err
	}
	return s.waitReady(s.opts.ReadyTimeout)
}

// Bin is the binary the current (or last) serve process runs.
func (s *Server) Bin() string { return s.bin }

// GroupAlive says whether anything in the last serve's process group is
// still running: serve itself, or a session child it started (children
// stay in serve's group). After Shutdown it is the "no child outlives
// serve" check.
func (s *Server) GroupAlive() bool { return groupAlive(s.cmd) }

// Build is GET /api/health's X-Bough-Build header: the build answering.
func (s *Server) Build(ctx context.Context) (string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, s.URL+"/api/health", nil)
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	resp, err := s.client.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusOK {
		return "", &APIError{Status: resp.StatusCode, Msg: resp.Status}
	}
	return resp.Header.Get(serve.BuildHeader), nil
}

// Binary is the bough binary servers run: BOUGH_BIN when set, else the
// build testbin shares with every suite of the run.
func Binary(t testing.TB) string {
	t.Helper()
	bin, err := testbin.Path()
	if err != nil {
		t.Fatal(err)
	}
	return bin
}

// Main runs the tests. The binary is testbin's cache entry, reused by
// later runs, so there is nothing to remove afterwards; it stays so
// callers' TestMain need not change.
func Main(m *testing.M) {
	os.Exit(m.Run())
}

// Start launches a server and registers Close as a cleanup. It fails the
// test when the server does not become healthy.
func Start(t testing.TB, opts Options) *Server {
	t.Helper()
	bin := Binary(t)
	if opts.Config == "" {
		opts.Config = DefaultConfig
	}
	if opts.ReadyTimeout == 0 {
		opts.ReadyTimeout = 30 * time.Second
	}
	// A picked free port can be taken by a neighbour before serve binds
	// it; that start fails fast, and a fresh port is the whole fix.
	var lastErr error
	for range 3 {
		s, err := start(t, bin, opts)
		if err == nil {
			return s
		}
		lastErr = err
		if !strings.Contains(err.Error(), "address already in use") {
			break
		}
	}
	t.Fatal(lastErr)
	return nil
}

func start(t testing.TB, bin string, opts Options) (*Server, error) {
	// A short root under the system temp dir, not t.TempDir: sessions
	// open unix sockets under HOME, and macOS caps a socket path at 104
	// bytes, which a test-named TempDir path overruns.
	root, err := os.MkdirTemp("", "bst-")
	if err != nil {
		return nil, err
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	home := filepath.Join(root, "home")
	files := map[string]string{".bough/bough.yml": opts.Config}
	for k, v := range opts.Files {
		files[k] = v
	}
	for rel, body := range files {
		p := filepath.Join(home, rel)
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			return nil, err
		}
		if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
			return nil, err
		}
	}
	port, err := freePort()
	if err != nil {
		return nil, err
	}
	addr := fmt.Sprintf("127.0.0.1:%d", port)
	s := &Server{
		URL: "http://" + addr, Addr: addr, Root: root, Home: home,
		out: &safeBuf{}, client: &http.Client{}, opts: opts,
	}
	if err := s.launch(bin); err != nil {
		return nil, err
	}
	// Registered after the root's removal, so it runs first: the
	// process is gone before its HOME is deleted under it.
	t.Cleanup(func() {
		s.Close()
		if t.Failed() {
			t.Logf("servetest: %s output:\n%s", addr, s.Output())
		}
	})
	if err := s.waitReady(opts.ReadyTimeout); err != nil {
		s.Close()
		return nil, err
	}
	return s, nil
}

// childEnv is the caller's environment with HOME moved and every
// provider key dropped, so a config that forgets llm-echo fails
// instead of spending tokens. BOUGH_WEB_ADDR is pinned off the user's
// page server port for the same reason the e2e suite pins it.
func childEnv(home string, extra []string) []string {
	drop := map[string]bool{"HOME": true, "BOUGH_WEB_ADDR": true, "BOUGH_BIN": true}
	var env []string
	for _, kv := range os.Environ() {
		k, _, _ := strings.Cut(kv, "=")
		if drop[k] || strings.HasSuffix(k, "_API_KEY") || strings.HasSuffix(k, "_TOKEN") {
			continue
		}
		env = append(env, kv)
	}
	env = append(env, "HOME="+home, "BOUGH_WEB_ADDR=127.0.0.1:0")
	return append(env, extra...)
}

func (s *Server) waitReady(timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	last := "no token yet"
	for time.Now().Before(deadline) {
		select {
		case <-s.exited:
			return fmt.Errorf("servetest: serve exited before ready (%s):\n%s", s.cmd.ProcessState, s.Output())
		default:
		}
		if s.Token == "" {
			s.Token = serveclient.ReadToken(s.Home)
		}
		if s.Token != "" {
			var h struct {
				OK bool `json:"ok"`
			}
			err := s.do(context.Background(), http.MethodGet, "/api/health", nil, &h)
			if err == nil && h.OK {
				return nil
			}
			if err != nil {
				last = err.Error()
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	return fmt.Errorf("servetest: serve on %s not ready after %s (%s):\n%s", s.Addr, timeout, last, s.Output())
}

// Close stops the server: SIGTERM, which has serve end its sessions,
// then SIGKILL to the whole process group if it has not exited in 5s.
// Idempotent. It deletes nothing; the temp root goes with the test.
func (s *Server) Close() { s.close.Do(s.Shutdown) }

// Output is everything the server wrote to stdout and stderr so far.
func (s *Server) Output() string { return s.out.String() }

// Dir makes a directory under the server's root, for a session's cwd.
func (s *Server) Dir(t testing.TB, name string) string {
	t.Helper()
	p := filepath.Join(s.Root, name)
	if err := os.MkdirAll(p, 0o755); err != nil {
		t.Fatal(err)
	}
	return p
}

// APIError is a non-2xx answer, with the server's error text.
type APIError struct {
	Status int
	Msg    string
}

func (e *APIError) Error() string { return fmt.Sprintf("servetest: %d: %s", e.Status, e.Msg) }

func (s *Server) do(ctx context.Context, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, s.URL+path, rd)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := s.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	if resp.StatusCode/100 != 2 {
		var e struct {
			Error string `json:"error"`
		}
		if json.Unmarshal(raw, &e) != nil || e.Error == "" {
			e.Error = strings.TrimSpace(string(raw))
		}
		return &APIError{Status: resp.StatusCode, Msg: e.Error}
	}
	if out == nil {
		return nil
	}
	if err := json.Unmarshal(raw, out); err != nil {
		return fmt.Errorf("servetest: %s %s: decode: %w: %s", method, path, err, raw)
	}
	return nil
}

type rowReply struct {
	Session serve.Row `json:"session"`
}

// CreateSession is POST /api/sessions for a local session in cwd,
// starting it on prompt ("" starts it idle).
func (s *Server) CreateSession(ctx context.Context, cwd, prompt string) (serve.Row, error) {
	var r rowReply
	err := s.do(ctx, http.MethodPost, "/api/sessions", map[string]string{"cwd": cwd, "prompt": prompt}, &r)
	return r.Session, err
}

// Prompt is POST /api/sessions/{id}/prompt.
func (s *Server) Prompt(ctx context.Context, id, text string) error {
	return s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/prompt", map[string]string{"text": text}, nil)
}

// GetSession is GET /api/sessions/{id}: the row and the whole transcript.
func (s *Server) GetSession(ctx context.Context, id string) (serve.Row, []serve.Line, error) {
	var r struct {
		Session serve.Row    `json:"session"`
		Entries []serve.Line `json:"entries"`
	}
	err := s.do(ctx, http.MethodGet, "/api/sessions/"+url.PathEscape(id), nil, &r)
	return r.Session, r.Entries, err
}

// ListSessions is GET /api/sessions, newest first; all includes archived.
func (s *Server) ListSessions(ctx context.Context, all bool) ([]serve.Row, error) {
	path := "/api/sessions"
	if all {
		path += "?all=1"
	}
	var r struct {
		Sessions []serve.Row `json:"sessions"`
	}
	err := s.do(ctx, http.MethodGet, path, nil, &r)
	return r.Sessions, err
}

// Archive is POST /api/sessions/{id}/archive.
func (s *Server) Archive(ctx context.Context, id string) (serve.Row, error) {
	var r rowReply
	err := s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/archive", nil, &r)
	return r.Session, err
}

// Unarchive is POST /api/sessions/{id}/unarchive.
func (s *Server) Unarchive(ctx context.Context, id string) (serve.Row, error) {
	var r rowReply
	err := s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/unarchive", nil, &r)
	return r.Session, err
}

// Ack is POST /api/sessions/{id}/ack: what the page sends when an
// unseen finish or a failure is on screen, clearing unseen and trouble.
func (s *Server) Ack(ctx context.Context, id string) (serve.Row, error) {
	var r rowReply
	err := s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/ack", nil, &r)
	return r.Session, err
}

// Interrupt is POST /api/sessions/{id}/interrupt: the composer's stop.
func (s *Server) Interrupt(ctx context.Context, id string) error {
	return s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/interrupt", nil, nil)
}

// Effort is POST /api/sessions/{id}/effort. It reaches the child as a
// "/think" line, so it also spawns one for a session with no child
// without starting a turn.
func (s *Server) Effort(ctx context.Context, id, level string) error {
	return s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/effort", map[string]string{"effort": level}, nil)
}

// PID is the serve process's pid; its session children are its own
// children, which a test that pauses one finds through it.
func (s *Server) PID() int { return s.cmd.Process.Pid }

// Rename is POST /api/sessions/{id}/rename.
func (s *Server) Rename(ctx context.Context, id, title string) error {
	return s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/rename", map[string]string{"title": title}, nil)
}

// Answer is POST /api/sessions/{id}/answer: the reply to a pending ask.
func (s *Server) Answer(ctx context.Context, id, text string) error {
	return s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/answer", map[string]string{"text": text}, nil)
}

// CreateAgent is POST /api/sessions with spawnedBy: parent starting a
// background agent on prompt, as tools.spawn({background}) does.
// maxRunning is the running cap the request sets (0 = serve's default);
// queued says it waits behind that cap.
func (s *Server) CreateAgent(ctx context.Context, parent, prompt string, maxRunning, maxPerSession int) (row serve.Row, queued bool, err error) {
	var r struct {
		Session serve.Row `json:"session"`
		Queued  bool      `json:"queued"`
	}
	err = s.do(ctx, http.MethodPost, "/api/sessions", map[string]any{
		"spawnedBy": parent, "prompt": prompt, "maxRunning": maxRunning, "maxPerSession": maxPerSession,
	}, &r)
	return r.Session, r.Queued, err
}

// Stop is POST /api/sessions/{id}/stop, the Work panel's stop. It
// answers what the session was: "running", "queued" or "idle".
func (s *Server) Stop(ctx context.Context, id string) (string, error) {
	var r struct {
		Was string `json:"was"`
	}
	err := s.do(ctx, http.MethodPost, "/api/sessions/"+url.PathEscape(id)+"/stop", map[string]string{}, &r)
	return r.Was, err
}

// WaitSession polls the session's row until ok holds or ctx ends; the
// error then carries the last row seen.
func (s *Server) WaitSession(ctx context.Context, id string, ok func(serve.Row) bool) (serve.Row, error) {
	var last serve.Row
	var lastErr error
	for {
		row, _, err := s.GetSession(ctx, id)
		if err == nil {
			last = row
			if ok(row) {
				return row, nil
			}
		}
		lastErr = err
		select {
		case <-ctx.Done():
			return last, fmt.Errorf("servetest: session %s: %w (last status %q, last error %v)", id, ctx.Err(), last.Status, lastErr)
		case <-time.After(100 * time.Millisecond):
		}
	}
}

// Stream reads one session's server-sent events.
type Stream struct {
	body   io.ReadCloser
	sc     *bufio.Scanner
	cancel context.CancelFunc
}

// Events opens GET /api/sessions/{id}/events. The stream replays the
// session's recent events first, then follows live ones.
func (s *Server) Events(ctx context.Context, id string) (*Stream, error) {
	ctx, cancel := context.WithCancel(ctx)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, s.URL+"/api/sessions/"+url.PathEscape(id)+"/events", nil)
	if err != nil {
		cancel()
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+s.Token)
	resp, err := s.client.Do(req)
	if err != nil {
		cancel()
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		raw, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		cancel()
		return nil, &APIError{Status: resp.StatusCode, Msg: strings.TrimSpace(string(raw))}
	}
	sc := bufio.NewScanner(resp.Body)
	// An event carries a whole tool output; the default 64 KiB line cap
	// would end the stream on the first big one.
	sc.Buffer(make([]byte, 0, 64<<10), 16<<20)
	return &Stream{body: resp.Body, sc: sc, cancel: cancel}, nil
}

// Next blocks for the next event. Comments (": ping") are skipped; the
// stream's end or its context's is an error.
func (st *Stream) Next() (serve.Event, error) {
	var data strings.Builder
	for st.sc.Scan() {
		line := st.sc.Text()
		switch {
		case line == "":
			if data.Len() == 0 {
				continue
			}
			var ev serve.Event
			if err := json.Unmarshal([]byte(data.String()), &ev); err != nil {
				return ev, fmt.Errorf("servetest: event %q: %w", data.String(), err)
			}
			return ev, nil
		case strings.HasPrefix(line, "data: "):
			data.WriteString(strings.TrimPrefix(line, "data: "))
		}
	}
	if err := st.sc.Err(); err != nil {
		return serve.Event{}, err
	}
	return serve.Event{}, io.EOF
}

// WaitFor reads until an event satisfies ok. ctx bounds the wait even
// though the stream has its own: closing the body is what unblocks a
// read.
func (st *Stream) WaitFor(ctx context.Context, ok func(serve.Event) bool) (serve.Event, error) {
	stop := context.AfterFunc(ctx, st.cancel)
	defer stop()
	for {
		ev, err := st.Next()
		if err != nil {
			if ctx.Err() != nil {
				return ev, ctx.Err()
			}
			return ev, err
		}
		if ok(ev) {
			return ev, nil
		}
	}
}

// Close ends the stream.
func (st *Stream) Close() {
	st.cancel()
	st.body.Close()
}

func freePort() (int, error) {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0, err
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port, nil
}

type safeBuf struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *safeBuf) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *safeBuf) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}
