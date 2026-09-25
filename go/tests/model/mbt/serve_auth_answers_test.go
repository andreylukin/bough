//go:build !windows

package mbt

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net"
	"net/http"
	"net/http/cookiejar"
	"net/http/httputil"
	"net/url"
	"os"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serveclient"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/serve_auth_answers.fizz against real serves: the Room role is
// serve A with its token file, one browser (a cookie jar shared by tab A
// and a tab on serve B, keyed by host and not by port, as a browser's
// is), the reverse proxy a --host name sits behind, a child's
// serveclient, and a second LoadToken racing a restart's create.
//
// Serve A comes in two builds of the same room: one on loopback with
// --host=bough.test (the tab reaches it on 127.0.0.1, through the proxy
// as bough.test, or as evil.test, a Host it does not answer to), and
// one bound 0.0.0.0 with --insecure-bind (the tab reaches it as
// lan.test). A path is walked on the one its LoadShell* names; a path
// that never loads uses the loopback one. Every *.test name dials
// 127.0.0.1: nothing leaves the machine.
//
// What is read off the real thing: got (the status the tab's request
// got back, or a refused connection, or the proxy's HTML 502), jar_a and
// jar_b (the cookie each serve reads out of the jar, against the tokens
// each serve has held), stream (the event stream's answer, and its end
// when serve goes away), disk and serve (the token file, the process),
// child (a real serveclient call after serveclient.Addr), loader (a real
// serveclient.LoadToken inside the window). shown and cat are the page's
// words for those answers, by the rules the spec owes (page_for,
// catalogue); the browser walk of this flow checks the page itself says
// them. No fault in serve makes /api/models answer 500, so
// OpenModelPicker5xx hands the page rule the {"error"} body writeErr
// writes, after checking /api answers ok.
//
// ServeRestart with the token file gone is split where LoadToken splits
// it: the adapter makes the O_EXCL create (the empty file) and serve is
// not started; TokenWritten writes a new token into it and starts serve,
// which reads it.

const (
	saaSpec     = "serve_auth_answers"
	saaHostName = "bough.test" // the --host name the proxy forwards
)

// saaServe is one serve A build: its server, the session the tab opens,
// and every token it has held, oldest first.
type saaServe struct {
	s      *servetest.Server
	sid    string
	tokens []string
	down   bool // stopped inside the create-then-write window
}

func (r *saaServe) port() string { _, p, _ := net.SplitHostPort(r.s.Addr); return p }

type saaStream struct {
	cancel context.CancelFunc
	done   chan struct{}
}

type saaAdapter struct {
	t    *testing.T
	gate gate

	loop, remote, cur *saaServe
	b                 *servetest.Server

	// The proxy in front of loop, on a fixed port so it can come back.
	proxyAddr string
	proxyLn   net.Listener
	proxy     string // up | down | back

	jar  *cookiejar.Jar
	http *http.Client

	via, got, shown, cat, child, loader, stream string
	other                                       bool
	ev                                          *saaStream
	loadRes                                     chan error // the racing LoadToken, nil when none

	// wrongRestart is TestServeAuthAnswersCatchesWrongAdapter's bug: a
	// restart with the token file gone lets serve mint its own token and
	// come straight up, so the window never shows.
	wrongRestart bool
}

func newSAAAdapter(t *testing.T) *saaAdapter {
	a := &saaAdapter{t: t}
	a.loop = a.startA(servetest.Options{Config: controlConfig, Args: []string{"--host=" + saaHostName}})
	a.remote = a.startA(servetest.Options{Config: controlConfig, Args: []string{"--insecure-bind"}, Bind: "0.0.0.0"})
	a.b = servetest.Start(t, servetest.Options{Config: controlConfig})
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	a.proxyAddr = ln.Addr().String()
	a.serveProxy(ln)
	t.Cleanup(func() {
		if a.proxyLn != nil {
			a.proxyLn.Close()
		}
		a.closeStream()
	})
	a.cur = a.loop
	return a
}

func (a *saaAdapter) startA(opts servetest.Options) *saaServe {
	s := servetest.Start(a.t, opts)
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(a.t, "work"), "")
	if err != nil {
		a.t.Fatal(err)
	}
	return &saaServe{s: s, sid: row.ID, tokens: []string{s.Token}}
}

// serveProxy is tailscale serve in front of the loopback build: it keeps
// the Host the browser used (the --host name) and answers its own HTML
// 502 when serve is not listening.
func (a *saaAdapter) serveProxy(ln net.Listener) {
	target, _ := url.Parse(a.loop.s.URL)
	rp := &httputil.ReverseProxy{
		Rewrite: func(pr *httputil.ProxyRequest) {
			pr.SetURL(target)
			pr.Out.Host = pr.In.Host
		},
		FlushInterval: -1,
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, _ error) {
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			w.WriteHeader(http.StatusBadGateway)
			io.WriteString(w, "<html><body>502 Bad Gateway</body></html>")
		},
	}
	a.proxyLn = ln
	go (&http.Server{Handler: rp}).Serve(ln)
}

// Init starts a walk on a fresh room: serve A up with its token file,
// no tab, a new browser, the proxy up, the child's first call made.
func (a *saaAdapter) Init() error { return a.initOn(a.loop) }

func (a *saaAdapter) initOn(r *saaServe) error {
	a.gate.reset()
	a.closeStream()
	a.cur = r
	if err := a.restore(r); err != nil {
		return err
	}
	a.drainLoader()
	if a.proxy == "down" || a.proxyLn == nil {
		ln, err := net.Listen("tcp", a.proxyAddr)
		if err != nil {
			return fmt.Errorf("proxy: %w", err)
		}
		a.serveProxy(ln)
	}
	a.proxy = "up"
	jar, _ := cookiejar.New(nil)
	a.jar = jar
	a.http = &http.Client{
		Jar: jar,
		// A tab's requests do not share a connection a proxy that went
		// down is still holding; the open event stream keeps its own.
		Transport: &http.Transport{DisableKeepAlives: true, DialContext: saaDial},
		Timeout:   actionTimeout,
	}
	a.via, a.got, a.shown, a.cat, a.stream, a.loader, a.other = "none", "", "blank", "none", "none", "none", false
	if err := a.childCall(); err != nil {
		return err
	}
	if a.child != "fresh" {
		return fmt.Errorf("the child's first call was %s", a.child)
	}
	return nil
}

// restore brings a serve A build back to up with its token on disk,
// from wherever a past walk left it.
func (a *saaAdapter) restore(r *saaServe) error {
	p := serveclient.TokenPath(r.s.Home)
	if r.down {
		if serveclient.ReadToken(r.s.Home) == "" {
			if err := os.WriteFile(p, []byte(newToken()+"\n"), 0o600); err != nil {
				return err
			}
		}
		return a.resume(r)
	}
	if serveclient.ReadToken(r.s.Home) == "" {
		return os.WriteFile(p, []byte(r.s.Token+"\n"), 0o600)
	}
	return nil
}

func (a *saaAdapter) resume(r *saaServe) error {
	r.s.Token = "" // Resume re-reads it from the file
	if err := r.s.Resume(""); err != nil {
		return err
	}
	r.down = false
	if r.s.Token != r.tokens[len(r.tokens)-1] {
		r.tokens = append(r.tokens, r.s.Token)
	}
	return nil
}

func newToken() string {
	b := make([]byte, 32)
	rand.Read(b)
	return hex.EncodeToString(b)
}

// saaDial sends every *.test name to 127.0.0.1.
func saaDial(ctx context.Context, network, addr string) (net.Conn, error) {
	if h, p, err := net.SplitHostPort(addr); err == nil && strings.HasSuffix(h, ".test") {
		addr = net.JoinHostPort("127.0.0.1", p)
	}
	return (&net.Dialer{}).DialContext(ctx, network, addr)
}

func (a *saaAdapter) Cleanup() error { return nil }

func (a *saaAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Room", Index: 0}: a}, nil
}

func (a *saaAdapter) GetState() (map[string]any, error) {
	serveSt := "up"
	if a.cur.down {
		serveSt = "starting"
	}
	disk, err := a.disk()
	if err != nil {
		return nil, err
	}
	stream := a.stream
	if stream == "open" && a.ev != nil {
		select {
		case <-a.ev.done:
			stream = "ended by itself"
		default:
		}
	}
	return map[string]any{
		"via": a.via, "proxy": a.proxy, "serve": serveSt, "disk": disk,
		"jar_a": a.jarA(), "other": a.other, "jar_b": a.jarB(),
		"got": a.got, "shown": a.shown, "stream": stream, "cat": a.cat,
		"child": a.child, "loader": a.loaderState(),
	}, nil
}

func (a *saaAdapter) disk() (string, error) {
	b, err := os.ReadFile(serveclient.TokenPath(a.cur.s.Home))
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return "missing", nil
	case err != nil:
		return "", err
	case strings.TrimSpace(string(b)) == "":
		return "empty", nil
	}
	return "ok", nil
}

// tabURL is where tab A points for via.
func (a *saaAdapter) tabURL() string {
	switch a.via {
	case "proxy":
		_, p, _ := net.SplitHostPort(a.proxyAddr)
		return "http://" + saaHostName + ":" + p
	case "unknown":
		return "http://evil.test:" + a.cur.port()
	case "remote":
		return "http://lan.test:" + a.cur.port()
	}
	return a.cur.s.URL
}

// cookieFor is the token serve on port reads out of what the jar sends
// to u: the cookie Guard looks at.
func (a *saaAdapter) cookieFor(u, port string) (string, bool) {
	pu, _ := url.Parse(u)
	byName := map[string]string{}
	for _, c := range a.jar.Cookies(pu) {
		byName[c.Name] = c.Value
	}
	for _, n := range []string{serve.CookieName(port), serve.TokenCookie} {
		if v, ok := byName[n]; ok {
			return v, true
		}
	}
	return "", false
}

func (a *saaAdapter) jarA() string {
	v, ok := a.cookieFor(a.tabURL(), a.cur.port())
	if !ok {
		return "none"
	}
	for i, tok := range a.cur.tokens {
		if v == tok {
			if i == len(a.cur.tokens)-1 {
				return "fresh"
			}
			return "stale"
		}
	}
	if v == a.b.Token {
		return "other"
	}
	return "unknown token " + v
}

// jarB is what tab B sends to serve B; before that tab is loaded there
// is no tab B to send anything.
func (a *saaAdapter) jarB() string {
	if !a.other {
		return "none"
	}
	_, p, _ := net.SplitHostPort(a.b.Addr)
	v, ok := a.cookieFor(a.b.URL, p)
	if !ok {
		return "none"
	}
	if v == a.b.Token {
		return "fresh"
	}
	for _, r := range []*saaServe{a.loop, a.remote} {
		for _, tok := range r.tokens {
			if v == tok {
				return "other"
			}
		}
	}
	return "unknown token " + v
}

// loaderState is the racing LoadToken: waiting while it has not
// returned, error once it failed. One that returned a token before the
// write had none to return.
func (a *saaAdapter) loaderState() string {
	if a.loader != "waiting" {
		return a.loader
	}
	select {
	case err := <-a.loadRes:
		a.loadRes, a.loader = nil, "returned before the write"
		if err != nil {
			a.loader = "error"
		}
	default:
	}
	return a.loader
}

// answer is what a response (or its absence) is to the tab.
func saaAnswer(resp *http.Response, err error) string {
	if err != nil {
		return "net"
	}
	switch {
	case resp.StatusCode == http.StatusOK:
		return "ok"
	case resp.StatusCode == http.StatusUnauthorized:
		return "401"
	case resp.StatusCode == http.StatusForbidden:
		return "403"
	case resp.StatusCode == http.StatusBadGateway && strings.HasPrefix(resp.Header.Get("Content-Type"), "text/html"):
		return "html502"
	}
	return fmt.Sprintf("status %d", resp.StatusCode)
}

// get is tab A's request for path; the body is read and closed.
func (a *saaAdapter) get(path string) (string, []byte) {
	resp, err := a.http.Get(a.tabURL() + path)
	if err != nil {
		return saaAnswer(nil, err), nil
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	return saaAnswer(resp, nil), body
}

// pageFor is the page's word for an answer (the spec's page_for).
func pageFor(ans string) string {
	switch ans {
	case "ok":
		return "ok"
	case "401":
		return "reauth"
	case "403":
		return "refused"
	}
	return "down"
}

func (a *saaAdapter) load(v string) error {
	a.closeStream()
	a.via = v
	a.get("/")
	a.stream, a.cat = "none", "none"
	a.got, _ = a.get("/api/sessions")
	a.shown = pageFor(a.got)
	return nil
}

func (a *saaAdapter) LoadShellLoopback() error {
	if !a.gate.pass(a.via == "none") {
		return nil
	}
	return a.load("loopback")
}

func (a *saaAdapter) LoadShellProxy() error {
	if !a.gate.pass(a.via == "none") {
		return nil
	}
	return a.load("proxy")
}

func (a *saaAdapter) LoadShellUnknownHost() error {
	if !a.gate.pass(a.via == "none") {
		return nil
	}
	return a.load("unknown")
}

func (a *saaAdapter) LoadShellRemote() error {
	if !a.gate.pass(a.via == "none" && a.cur == a.remote) {
		return nil
	}
	return a.load("remote")
}

func (a *saaAdapter) Reload() error {
	if !a.gate.pass(a.via != "none" && !a.cur.down && (a.via != "proxy" || a.proxy != "down")) {
		return nil
	}
	return a.load(a.via)
}

func (a *saaAdapter) ListPoll() error {
	if !a.gate.pass(a.via == "loopback" || a.via == "proxy" || a.via == "remote") {
		return nil
	}
	a.got, _ = a.get("/api/sessions")
	a.shown = pageFor(a.got)
	return nil
}

// openStream is the EventSource's request: the stream is kept open, and
// done closes when serve (or the proxy) ends it.
func (a *saaAdapter) openStream() string {
	ctx, cancel := context.WithCancel(context.Background())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, a.tabURL()+"/api/sessions/"+a.cur.sid+"/events", nil)
	resp, err := a.http.Do(req)
	ans := saaAnswer(resp, err)
	if ans != "ok" {
		if resp != nil {
			resp.Body.Close()
		}
		cancel()
		return ans
	}
	ev := &saaStream{cancel: cancel, done: make(chan struct{})}
	go func() {
		io.Copy(io.Discard, resp.Body)
		resp.Body.Close()
		close(ev.done)
	}()
	a.ev = ev
	return ans
}

func (a *saaAdapter) closeStream() {
	if a.ev != nil {
		a.ev.cancel()
		<-a.ev.done
		a.ev = nil
	}
}

func (a *saaAdapter) Subscribe() error {
	if !a.gate.pass(a.shown == "ok" && a.stream == "none") {
		return nil
	}
	switch ans := a.openStream(); ans {
	case "ok":
		a.got, a.stream = ans, "open"
	case "401":
		a.stream, a.got = "closed", ans
		a.shown = pageFor(ans)
	default:
		a.got = ans
		a.shown = pageFor(ans)
	}
	return nil
}

func (a *saaAdapter) StreamReconnect() error {
	if !a.gate.pass(a.stream == "dropped" && !a.cur.down && (a.via != "proxy" || a.proxy != "down")) {
		return nil
	}
	a.closeStream()
	switch ans := a.openStream(); ans {
	case "ok":
		a.stream = "open"
	case "401":
		a.stream, a.got = "closed", ans
		a.shown = pageFor(ans)
	default:
		return fmt.Errorf("the stream reconnected to a serve that is up and got %s", ans)
	}
	return nil
}

func (a *saaAdapter) OpenModelPicker() error {
	if !a.gate.pass(a.shown == "ok" && a.cat != "ok") {
		return nil
	}
	ans, body := a.get("/api/models")
	if ans == "ok" {
		var c struct {
			Providers []json.RawMessage `json:"providers"`
		}
		if err := json.Unmarshal(body, &c); err != nil || c.Providers == nil {
			return fmt.Errorf("/api/models answered 200 without a catalogue: %s", body)
		}
	}
	a.catalogue(ans)
	return nil
}

func (a *saaAdapter) OpenModelPicker5xx() error {
	enabled := a.shown == "ok" && a.cat != "ok"
	if enabled {
		ans, _ := a.get("/api/models")
		enabled = ans == "ok"
	}
	if !a.gate.pass(enabled) {
		return nil
	}
	a.catalogue("5xx")
	return nil
}

// catalogue is useCatalogue's handling of the answer, as the spec owes
// it: a body is a catalogue only when res.ok.
func (a *saaAdapter) catalogue(ans string) {
	if ans == "ok" {
		a.cat = "ok"
		return
	}
	a.cat = "failed"
	if ans != "5xx" {
		a.got = ans
		a.shown = pageFor(ans)
	}
}

func (a *saaAdapter) OpenOtherServe() error {
	if !a.gate.pass(a.via == "loopback" && !a.other) {
		return nil
	}
	resp, err := a.http.Get(a.b.URL + "/")
	if err != nil {
		return err
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("serve B's page answered %s", resp.Status)
	}
	a.other = true
	return nil
}

func (a *saaAdapter) DeleteTokenFile() error {
	d, err := a.disk()
	if err != nil {
		return err
	}
	if !a.gate.pass(d == "ok") {
		return nil
	}
	return os.Remove(serveclient.TokenPath(a.cur.s.Home))
}

// ServeRestart is SIGTERM and a new start. With the token file in
// place serve comes back on the same token; without one the start is
// held in LoadToken's window: the file is created, empty, and serve is
// not listening yet.
func (a *saaAdapter) ServeRestart() error {
	if !a.gate.pass(!a.cur.down) {
		return nil
	}
	d, err := a.disk()
	if err != nil {
		return err
	}
	a.cur.s.Shutdown()
	if a.stream == "open" {
		select {
		case <-a.ev.done:
			a.stream = "dropped"
		case <-time.After(actionTimeout):
			return errors.New("the event stream outlived serve")
		}
	}
	if d == "missing" && !a.wrongRestart {
		f, err := os.OpenFile(serveclient.TokenPath(a.cur.s.Home), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
		if err != nil {
			return err
		}
		f.Close()
		a.cur.down = true
		return nil
	}
	return a.resume(a.cur)
}

func (a *saaAdapter) TokenWritten() error {
	d, err := a.disk()
	if err != nil {
		return err
	}
	if !a.gate.pass(d == "empty") {
		return nil
	}
	tok := newToken()
	f, err := os.OpenFile(serveclient.TokenPath(a.cur.s.Home), os.O_WRONLY, 0)
	if err != nil {
		return err
	}
	_, err = f.WriteString(tok + "\n")
	f.Close()
	if err != nil {
		return err
	}
	if err := a.resume(a.cur); err != nil {
		return err
	}
	if a.cur.s.Token != tok {
		return fmt.Errorf("serve started on %q, not the token written", a.cur.s.Token)
	}
	if a.child == "fresh" {
		a.child = "stale"
	}
	if a.loader == "waiting" {
		select {
		case err := <-a.loadRes:
			a.loadRes, a.loader = nil, "none"
			if err != nil {
				a.loader = "error"
			}
		case <-time.After(actionTimeout):
			return errors.New("the racing LoadToken never returned")
		}
	}
	return nil
}

// ConcurrentLoadToken is another LoadToken inside the window: it either
// fails at once, or waits and must come back with the token written.
func (a *saaAdapter) ConcurrentLoadToken() error {
	d, err := a.disk()
	if err != nil {
		return err
	}
	if !a.gate.pass(d == "empty" && a.loader == "none") {
		return nil
	}
	home := a.cur.s.Home
	res := make(chan error, 1)
	go func() {
		tok, err := serveclient.LoadToken(home)
		if err == nil && tok != serveclient.ReadToken(home) {
			err = fmt.Errorf("LoadToken returned %q, the file holds another", tok)
		}
		res <- err
	}()
	a.loadRes, a.loader = res, "waiting"
	// Today's failure is immediate; give it the time to show.
	time.Sleep(300 * time.Millisecond)
	a.loaderState()
	return nil
}

// drainLoader waits out a racing LoadToken a past walk left, after
// restore wrote the token it waits for.
func (a *saaAdapter) drainLoader() {
	if a.loadRes != nil {
		select {
		case <-a.loadRes:
		case <-time.After(actionTimeout):
		}
	}
	a.loadRes, a.loader = nil, "none"
}

// childCall is a child session's call as serveclient makes it: Addr
// (which re-reads the token file) and a request with the Bearer it
// holds. An agent that is not the caller's is a 4xx that is not 401.
func (a *saaAdapter) childCall() error {
	base, err := serveclient.Addr(a.cur.s.Home)
	if err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	_, err = (&serveclient.Client{Base: base, Parent: "nobody"}).Agent(ctx, a.cur.sid)
	var ae *serveclient.APIError
	switch {
	case errors.As(err, &ae) && ae.Code == http.StatusUnauthorized:
		a.child = "refused_intact"
	case err == nil || errors.As(err, &ae):
		a.child = "fresh"
	default:
		return err
	}
	return nil
}

func (a *saaAdapter) ChildCall() error {
	d, err := a.disk()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.cur.down && d == "ok") {
		return nil
	}
	return a.childCall()
}

// ProxyDown stops the proxy listening; a stream it is carrying stays.
func (a *saaAdapter) ProxyDown() error {
	if !a.gate.pass(a.via == "proxy" && a.proxy == "up") {
		return nil
	}
	a.proxyLn.Close()
	a.proxy = "down"
	return nil
}

func (a *saaAdapter) ProxyUp() error {
	if !a.gate.pass(a.via == "proxy" && a.proxy == "down") {
		return nil
	}
	ln, err := net.Listen("tcp", a.proxyAddr)
	if err != nil {
		return fmt.Errorf("proxy: %w", err)
	}
	a.serveProxy(ln)
	a.proxy = "back"
	return nil
}

var saaActions = map[string]func(*saaAdapter) error{
	"LoadShellLoopback":    (*saaAdapter).LoadShellLoopback,
	"LoadShellProxy":       (*saaAdapter).LoadShellProxy,
	"LoadShellUnknownHost": (*saaAdapter).LoadShellUnknownHost,
	"LoadShellRemote":      (*saaAdapter).LoadShellRemote,
	"Reload":               (*saaAdapter).Reload,
	"ListPoll":             (*saaAdapter).ListPoll,
	"Subscribe":            (*saaAdapter).Subscribe,
	"StreamReconnect":      (*saaAdapter).StreamReconnect,
	"OpenModelPicker":      (*saaAdapter).OpenModelPicker,
	"OpenModelPicker5xx":   (*saaAdapter).OpenModelPicker5xx,
	"OpenOtherServe":       (*saaAdapter).OpenOtherServe,
	"DeleteTokenFile":      (*saaAdapter).DeleteTokenFile,
	"ServeRestart":         (*saaAdapter).ServeRestart,
	"TokenWritten":         (*saaAdapter).TokenWritten,
	"ConcurrentLoadToken":  (*saaAdapter).ConcurrentLoadToken,
	"ChildCall":            (*saaAdapter).ChildCall,
	"ProxyDown":            (*saaAdapter).ProxyDown,
	"ProxyUp":              (*saaAdapter).ProxyUp,
}

// saaWalk walks every path against the adapter and returns the first
// step whose state is not the spec's.
func saaWalk(a *saaAdapter, b []byte) (int, error) {
	var f struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &f); err != nil {
		return 0, err
	}
	for i, p := range f.Paths {
		room := a.loop
		for _, s := range p.Trace {
			if s.Action == "Room#0.LoadShellRemote" {
				room = a.remote
			}
		}
		for j, step := range p.Trace {
			var err error
			if j == 0 {
				err = a.initOn(room)
			} else {
				name := strings.TrimPrefix(step.Action, "Room#0.")
				fn, ok := saaActions[name]
				if !ok {
					return 0, fmt.Errorf("path %d step %d: no action %s", i, j, step.Action)
				}
				err = fn(a)
				if err == nil && a.gate.off {
					err = errors.New("the adapter found it disabled")
				}
			}
			if err != nil {
				return 0, fmt.Errorf("path %d step %d (%s): %v", i, j, step.Action, err)
			}
			got, err := a.GetState()
			if err != nil {
				return 0, fmt.Errorf("path %d step %d (%s): state: %v", i, j, step.Action, err)
			}
			if d := roomDiff(step.State, got); d != "" {
				var trail []string
				for _, s := range p.Trace[1 : j+1] {
					trail = append(trail, strings.TrimPrefix(s.Action, "Room#0."))
				}
				return 0, fmt.Errorf("path %d step %d (%s): %s\nafter: %s", i, j, step.Action, d, strings.Join(trail, " "))
			}
		}
	}
	return len(f.Paths), nil
}

// serveAuthAnswersHistory reads the tab's session transcript. Nothing
// this flow does is a turn: loads, polls, streams, restarts and token
// files leave the transcript as its create wrote it, so the whole
// trace is Init, and a transcript with anything a turn writes is not.
func serveAuthAnswersHistory(entries []history.Entry) []tracecheck.Step {
	steps := []tracecheck.Step{{Action: "Init", State: map[string]any{"Room#0.via": "none", "Room#0.serve": "up"}}}
	for _, e := range entries {
		if e.Kind == "input" || e.Kind == "done" {
			steps = append(steps, tracecheck.Step{Action: "Room#0." + e.Kind})
		}
	}
	return steps
}

func init() { historyProjections[saaSpec] = serveAuthAnswersHistory }

func TestServeAuthAnswersPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSON(saaSpec)
	if err != nil {
		t.Fatal(err)
	}
	a := newSAAAdapter(t)
	start := time.Now()
	n, err := saaWalk(a, b)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%d paths in %s", n, time.Since(start).Round(time.Second))
	g, err := tracecheck.Load(fizzCheck(t, saaSpec))
	if err != nil {
		t.Fatal(err)
	}
	for _, r := range []*saaServe{a.loop, a.remote} {
		checkHistory(t, g, sessionHistory(t, r.s.Home, r.sid), serveAuthAnswersHistory)
	}
}

// A turn in the tab's session is not something this flow's steps do.
func TestServeAuthAnswersHistoryProjection(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(fizzCheck(t, saaSpec))
	if err != nil {
		t.Fatal(err)
	}
	e := func(kind string) history.Entry { return history.Entry{Kind: kind} }
	if v := g.Check(serveAuthAnswersHistory([]history.Entry{e("meta")})); v != nil {
		t.Fatalf("a created session: %v", v)
	}
	if v := g.Check(serveAuthAnswersHistory([]history.Entry{e("meta"), e("input"), e("done")})); v == nil {
		t.Fatal("a transcript with a turn passed the trace check")
	}
}

// A restart that lets serve mint its own token with the file gone skips
// the create-then-write window; it shows on the ServeRestart links out
// of a deleted token file, so every link is walked.
func TestServeAuthAnswersCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	b, err := pathsJSONCover(saaSpec, tracecheck.CoverTransitions)
	if err != nil {
		t.Fatal(err)
	}
	a := newSAAAdapter(t)
	a.wrongRestart = true
	if _, err := saaWalk(a, b); err == nil {
		t.Fatal("a walk whose restart skips the token window passed; the walk is not checking state")
	} else {
		t.Logf("caught: %v", err)
	}
}
