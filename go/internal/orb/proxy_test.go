package orb

import (
	"bufio"
	"encoding/base64"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

func TestRelayRunsOnlyMCPOnHost(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	fake := filepath.Join(dir, "bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho \"host: $*\"; cat; echo oops >&2; exit 3\n"), 0o755)
	p, err := startProxyBin("127.0.0.1", "", "", fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	client, closeIdle := ownClient()
	defer closeIdle()

	// The framing the shim writes: one base64 arg per line, a blank
	// line, then base64 stdin.
	frame := func(stdin string, args ...string) string {
		var b strings.Builder
		for _, a := range args {
			b.WriteString(base64.StdEncoding.EncodeToString([]byte(a)) + "\n")
		}
		b.WriteString("\n" + base64.StdEncoding.EncodeToString([]byte(stdin)))
		return b.String()
	}
	post := func(body string) (*http.Response, string, string, int) {
		resp, err := client.Post(p.URL()+"/bough/exec", "text/plain", strings.NewReader(body))
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		raw, _ := io.ReadAll(resp.Body)
		if resp.StatusCode != 200 {
			return resp, "", "", 0
		}
		lines := strings.Split(strings.TrimSuffix(string(raw), "\n"), "\n")
		if len(lines) != 3 {
			t.Fatalf("relay body: %q", raw)
		}
		exit, _ := strconv.Atoi(lines[0])
		out, _ := base64.StdEncoding.DecodeString(lines[1])
		errb, _ := base64.StdEncoding.DecodeString(lines[2])
		return resp, string(out), string(errb), exit
	}
	resp, out, errs, exit := post(frame("in", "mcp", "call", "linear-server/x", "{}"))
	if resp.StatusCode != 200 || out != "host: mcp call linear-server/x {}\nin" || errs != "oops\n" || exit != 3 {
		t.Fatalf("relay: %d out=%q err=%q exit=%d", resp.StatusCode, out, errs, exit)
	}
	// An argument carrying a newline and quotes survives the framing:
	// that is what base64 is there for.
	if _, out, _, _ := post(frame("", "mcp", "call", "x/y", "a\nb \"q\"")); !strings.Contains(out, "a\nb \"q\"") {
		t.Fatalf("argv with a newline did not survive: %q", out)
	}
	if resp, _, _, _ := post(frame("", "update")); resp.StatusCode != http.StatusForbidden {
		t.Fatalf("non-relayed command got %d", resp.StatusCode)
	}
	// browser is relayed too, so an orb can drive the host's browser.
	if resp, _, _, _ := post(frame("", "browser", "snapshot")); resp.StatusCode != 200 {
		t.Fatalf("browser not relayed: %d", resp.StatusCode)
	}
	// A result past Go's 2KB chunking threshold is still one sized body:
	// the shim reads three lines, and a chunked response put a chunk
	// size where the exit code goes (every big MCP result failed in an
	// orb with "base64: invalid input" and exit 1).
	big := strings.Repeat("x", 70_000)
	os.WriteFile(fake, []byte("#!/bin/sh\nprintf '%s' \""+big+"\"\n"), 0o755)
	resp, out, _, exit = post(frame("", "mcp", "call", "s/t"))
	if len(resp.TransferEncoding) != 0 || resp.ContentLength <= 0 || out != big || exit != 0 {
		t.Fatalf("big relay: te=%v len=%d out=%d exit=%d", resp.TransferEncoding, resp.ContentLength, len(out), exit)
	}
}

// ownClient is an HTTP client whose connections the test closes before
// the proxy's: http.DefaultClient can hold a spare dialed connection that
// never sends a request, and Shutdown waits on it until Close's one-second
// timeout, so the test took 1 s about every other run.
func ownClient() (*http.Client, func()) {
	tr := &http.Transport{}
	return &http.Client{Transport: tr}, tr.CloseIdleConnections
}

// fakeBough is a proxy's host bough that is the script at path.
func fakeBough(path string) func() (string, error) {
	return func() (string, error) { return path, nil }
}

func TestShimRelaysThroughHost(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho \"host: $*\"; exit 2\n"), 0o755)
	p, err := startProxyBin("127.0.0.1", "", "", fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	scratch := t.TempDir()
	if err := writeShim(scratch); err != nil {
		t.Fatal(err)
	}
	c := exec.Command(filepath.Join(shimDir(scratch), "bough"), "mcp", "list")
	c.Env = append(os.Environ(), "BOUGH_HOST="+p.URL())
	out, err := c.Output()
	if ee, ok := err.(*exec.ExitError); !ok || ee.ExitCode() != 2 || string(out) != "host: mcp list\n" {
		t.Fatalf("shim: %q %v", out, err)
	}
}

// The shim must not need an interpreter the image may not have. A
// project with its own Dockerfile had no python3, and the old shim died
// with "python3: not found" and nothing said why; PATH here is cut down
// to the bare coreutils an image is fair to assume.
func TestShimNeedsOnlyBashAndBase64(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho \"host: $*\"; cat; exit 0\n"), 0o755)
	p, err := startProxyBin("127.0.0.1", "", "", fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	scratch := t.TempDir()
	if err := writeShim(scratch); err != nil {
		t.Fatal(err)
	}
	// A PATH with only bash and base64 (plus the tr/sed the shim uses):
	// python3, curl, wget and nc are deliberately absent.
	bin := filepath.Join(t.TempDir(), "bin")
	if err := os.MkdirAll(bin, 0o755); err != nil {
		t.Fatal(err)
	}
	for _, tool := range []string{"bash", "base64", "tr", "sed"} {
		src, err := exec.LookPath(tool)
		if err != nil {
			t.Skipf("%s not installed", tool)
		}
		if err := os.Symlink(src, filepath.Join(bin, tool)); err != nil {
			t.Fatal(err)
		}
	}
	c := exec.Command(filepath.Join(shimDir(scratch), "bough"), "mcp", "call", "x/y", "a\nb")
	c.Env = []string{"BOUGH_HOST=" + p.URL(), "PATH=" + bin}
	c.Stdin = strings.NewReader("piped")
	out, err := c.Output()
	if err != nil {
		t.Fatalf("shim on a bare PATH: %v (%q)", err, out)
	}
	// The newline inside an argument came through, and so did stdin.
	if want := "host: mcp call x/y a\nb\npiped"; string(out) != want {
		t.Fatalf("shim: got %q want %q", out, want)
	}
}

// A request past the relay's 8 MiB is refused with a reason and exit 1,
// like every other refusal. The relay answers 400 and closes after
// reading 8 MiB, while the shim is still writing: bash died of SIGPIPE
// on the rest and the guest saw its call end with nothing said.
func TestShimExplainsTooLargeRequest(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not installed")
	}
	if !strings.Contains(shimScript, " -gt "+strconv.Itoa(relayMaxBody)+" ]") {
		t.Fatal("the shim's size check is not relayMaxBody")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho ran\n"), 0o755)
	p, err := startProxyBin("127.0.0.1", "", "", fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	scratch := t.TempDir()
	if err := writeShim(scratch); err != nil {
		t.Fatal(err)
	}
	c := exec.Command(filepath.Join(shimDir(scratch), "bough"), "mcp", "call", "x/y")
	c.Env = append(os.Environ(), "BOUGH_HOST="+p.URL())
	c.Stdin = strings.NewReader(strings.Repeat("x", relayMaxBody))
	var stderr strings.Builder
	c.Stderr = &stderr
	out, err := c.Output()
	ee, ok := err.(*exec.ExitError)
	if !ok || ee.ExitCode() != 1 || !strings.Contains(stderr.String(), "too large") || len(out) != 0 {
		t.Fatalf("oversized call: err %v, stdout %q, stderr %q", err, out, stderr.String())
	}
}

func TestProxyForwardsHTTPAndConnect(t *testing.T) {
	t.Parallel()
	up := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, "hello "+r.URL.Path)
	}))
	defer up.Close()
	p, err := startProxy("127.0.0.1", "", "")
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()

	pu, _ := url.Parse(p.URL())
	c := &http.Client{Transport: &http.Transport{Proxy: http.ProxyURL(pu)}}
	resp, err := c.Get(up.URL + "/plain")
	if err != nil {
		t.Fatal(err)
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if string(b) != "hello /plain" {
		t.Fatalf("plain: got %q", b)
	}

	conn, err := net.Dial("tcp", p.Addr())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	host := strings.TrimPrefix(up.URL, "http://")
	fmt.Fprintf(conn, "CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n", host, host)
	br := bufio.NewReader(conn)
	cr, err := http.ReadResponse(br, nil)
	if err != nil || cr.StatusCode != 200 {
		t.Fatalf("connect: %v %v", cr, err)
	}
	fmt.Fprintf(conn, "GET /tunnel HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", host)
	tr, err := http.ReadResponse(br, nil)
	if err != nil {
		t.Fatal(err)
	}
	b, _ = io.ReadAll(tr.Body)
	if string(b) != "hello /tunnel" {
		t.Fatalf("tunnel: got %q", b)
	}
}

func TestProxyRequiresToken(t *testing.T) {
	t.Parallel()
	up := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, "ok")
	}))
	defer up.Close()
	p, err := startProxy("127.0.0.1", "s3cret-token", "")
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	client, closeIdle := ownClient()
	defer closeIdle()
	if !strings.Contains(p.URL(), "bough:s3cret-token@") || strings.Contains(p.Addr(), "s3cret") {
		t.Fatalf("url %q addr %q", p.URL(), p.Addr())
	}
	get := func(proxyURL string) int {
		pu, _ := url.Parse(proxyURL)
		c := &http.Client{Transport: &http.Transport{Proxy: http.ProxyURL(pu)}}
		resp, err := c.Get(up.URL + "/x")
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		return resp.StatusCode
	}
	if code := get("http://" + p.Addr()); code != http.StatusProxyAuthRequired {
		t.Fatalf("no creds: %d", code)
	}
	if code := get("http://bough:wrong@" + p.Addr()); code != http.StatusProxyAuthRequired {
		t.Fatalf("wrong creds: %d", code)
	}
	if code := get(p.URL()); code != 200 {
		t.Fatalf("token: %d", code)
	}

	conn, err := net.Dial("tcp", p.Addr())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	host := strings.TrimPrefix(up.URL, "http://")
	fmt.Fprintf(conn, "CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n", host, host)
	if cr, err := http.ReadResponse(bufio.NewReader(conn), nil); err != nil || cr.StatusCode != http.StatusProxyAuthRequired {
		t.Fatalf("connect without token: %v %v", cr, err)
	}

	post := func(auth string) int {
		// A well-formed body for a command that is not relayed, so past
		// auth it is the command check that answers, not the parser.
		body := base64.StdEncoding.EncodeToString([]byte("update")) + "\n\n"
		req, _ := http.NewRequest("POST", "http://"+p.Addr()+"/bough/exec", strings.NewReader(body))
		if auth != "" {
			req.Header.Set("Authorization", auth)
		}
		resp, err := client.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		return resp.StatusCode
	}
	if code := post(""); code != http.StatusUnauthorized {
		t.Fatalf("relay without token: %d", code)
	}
	// Past auth, the command check answers.
	if code := post("Bearer s3cret-token"); code != http.StatusForbidden {
		t.Fatalf("relay with token: %d", code)
	}
}

func TestShimSendsToken(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho ok\n"), 0o755)
	p, err := startProxyBin("127.0.0.1", "tok-12345", "", fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	scratch := t.TempDir()
	writeShim(scratch)
	run := func(env ...string) (string, error) {
		c := exec.Command(filepath.Join(shimDir(scratch), "bough"), "mcp", "list")
		c.Env = append(os.Environ(), append(env, "BOUGH_HOST=http://"+p.Addr())...)
		out, err := c.CombinedOutput()
		return string(out), err
	}
	if out, err := run("BOUGH_ORB_TOKEN=tok-12345"); err != nil || out != "ok\n" {
		t.Fatalf("with token: %q %v", out, err)
	}
	if out, err := run("BOUGH_ORB_TOKEN="); err == nil || !strings.Contains(out, "recreate") {
		t.Fatalf("without token: %q %v", out, err)
	}
}

// relayCI posts `bough ci` args with a guest cwd and returns the status,
// and the fake host bough's stdout (its pwd and args) on a 200.
func relayCI(t *testing.T, p *proxy, cwd string, args ...string) (int, string) {
	t.Helper()
	var b strings.Builder
	for _, a := range args {
		b.WriteString(base64.StdEncoding.EncodeToString([]byte(a)) + "\n")
	}
	b.WriteString("\n")
	req, _ := http.NewRequest("POST", p.URL()+"/bough/exec", strings.NewReader(b.String()))
	if cwd != "" {
		req.Header.Set(relayCwdHeader, cwd)
	}
	client, closeIdle := ownClient()
	defer closeIdle()
	resp, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != 200 {
		return resp.StatusCode, string(raw)
	}
	lines := strings.Split(strings.TrimSuffix(string(raw), "\n"), "\n")
	out, _ := base64.StdEncoding.DecodeString(lines[1])
	return 200, string(out)
}

// A guest's `bough ci --no-wait` runs on the host in the guest's cwd,
// which is the same path: worktrees are mounted at their host paths.
func TestRelayCIRunsInGuestCwd(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	fake := filepath.Join(dir, "bough")
	os.WriteFile(fake, []byte("#!/bin/sh\npwd -P; echo \"$*\"\n"), 0o755)
	root := t.TempDir()
	wt := filepath.Join(root, "repo", "sub")
	os.MkdirAll(wt, 0o755)
	p, err := startProxyBin("127.0.0.1", "", root, fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	realWT, _ := filepath.EvalSymlinks(wt)
	for _, args := range [][]string{{"ci", "--no-wait"}, {"ci", "log", "vet"}, {"ci", "-no-wait=true", "--json"}} {
		code, out := relayCI(t, p, wt, args...)
		if code != 200 || !strings.HasPrefix(out, realWT+"\n") {
			t.Fatalf("%v: %d %q (want pwd %s)", args, code, out, realWT)
		}
	}
}

func TestRelayCIRejectsRunsAndCwdOutsideRoot(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	fake := filepath.Join(dir, "bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho ran\n"), 0o755)
	root := t.TempDir()
	p, err := startProxyBin("127.0.0.1", "", root, fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	for _, tc := range []struct {
		cwd  string
		args []string
		want string
	}{
		{root, []string{"ci"}, "cannot run checks from an orb"},
		{root, []string{"ci", "--no-wait=false"}, "cannot run checks from an orb"},
		{root, []string{"ci", "--no-wait", "--no-wait=false"}, "cannot run checks from an orb"},
		{root, []string{"ci", "--no-wait", "-no-wait=0"}, "cannot run checks from an orb"},
		{root, []string{"ci", "--no-wait", "--no-wait=bogus"}, "invalid boolean"},
		{root, []string{"ci", "--no-wait", "--bogus"}, "not defined"},
		{root, []string{"ci", "log", "vet", "--dir", "/"}, "--dir"},
		{root, []string{"ci", "log", "vet", "-dir=/"}, "--dir"},
		{root, []string{"ci", "--no-wait", "--dir", "/"}, "--dir"},
		{t.TempDir(), []string{"ci", "--no-wait"}, "outside the orb"},
		{filepath.Join(root, ".."), []string{"ci", "--no-wait"}, "outside the orb"},
		{"", []string{"ci", "--no-wait"}, "outside the orb"},
	} {
		code, body := relayCI(t, p, tc.cwd, tc.args...)
		if code != http.StatusForbidden || !strings.Contains(body, tc.want) {
			t.Errorf("%v from %q: %d %q (want 403 %q)", tc.args, tc.cwd, code, body, tc.want)
		}
	}
}

// The shim sends its working directory, so `bough ci` in the guest is
// about the checkout the agent's shell is in.
func TestShimSendsCwd(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\npwd -P\n"), 0o755)
	root := t.TempDir()
	p, err := startProxyBin("127.0.0.1", "", root, fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	scratch := t.TempDir()
	if err := writeShim(scratch); err != nil {
		t.Fatal(err)
	}
	c := exec.Command(filepath.Join(shimDir(scratch), "bough"), "ci", "--no-wait")
	c.Dir = root
	c.Env = append(os.Environ(), "BOUGH_HOST="+p.URL())
	out, err := c.Output()
	realRoot, _ := filepath.EvalSymlinks(root)
	if err != nil || strings.TrimSpace(string(out)) != realRoot {
		t.Fatalf("shim ci: %q %v (want %s)", out, err, realRoot)
	}
}

// A relayed command learns whose orb asked: `bough project restart` with
// no session defaults to the caller's and records the agent as asking.
func TestRelayPassesCallerSession(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("bash"); err != nil {
		t.Skip("bash not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho \"s=$BOUGH_SESSION r=$BOUGH_RELAYED b=$AGENT_BROWSER_SESSION\"\n"), 0o755)
	p, err := startProxyBin("127.0.0.1", "", t.TempDir(), fakeBough(fake))
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	scratch := t.TempDir()
	if err := writeShim(scratch); err != nil {
		t.Fatal(err)
	}
	c := exec.Command(filepath.Join(shimDir(scratch), "bough"), "project", "restart")
	c.Env = append(os.Environ(), "BOUGH_HOST="+p.URL(), "BOUGH_SESSION=sess-7")
	out, err := c.Output()
	if err != nil || string(out) != "s=sess-7 r=1 b=sess-7\n" {
		t.Fatalf("relayed env: %q %v", out, err)
	}
}
