package orb

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestRelayRunsOnlyMCPOnHost(t *testing.T) {
	// Not parallel: it swaps hostBough.
	dir := t.TempDir()
	fake := filepath.Join(dir, "bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho \"host: $*\"; cat; echo oops >&2; exit 3\n"), 0o755)
	prev := hostBough
	hostBough = func() (string, error) { return fake, nil }
	defer func() { hostBough = prev }()
	p, err := startProxy("127.0.0.1", "")
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()

	post := func(body string) (*http.Response, relayResponse) {
		resp, err := http.Post(p.URL()+"/bough/exec", "application/json", strings.NewReader(body))
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		var rr relayResponse
		json.NewDecoder(resp.Body).Decode(&rr)
		return resp, rr
	}
	resp, rr := post(`{"args":["mcp","call","linear-server/x","{}"],"stdin":"in"}`)
	if resp.StatusCode != 200 || rr.Stdout != "host: mcp call linear-server/x {}\nin" || rr.Stderr != "oops\n" || rr.Exit != 3 {
		t.Fatalf("relay: %d %+v", resp.StatusCode, rr)
	}
	if resp, _ := post(`{"args":["update"]}`); resp.StatusCode != http.StatusForbidden {
		t.Fatalf("non-mcp command got %d", resp.StatusCode)
	}
}

func TestShimRelaysThroughHost(t *testing.T) {
	if _, err := exec.LookPath("python3"); err != nil {
		t.Skip("python3 not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho \"host: $*\"; exit 2\n"), 0o755)
	prev := hostBough
	hostBough = func() (string, error) { return fake, nil }
	defer func() { hostBough = prev }()
	p, err := startProxy("127.0.0.1", "")
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

func TestProxyForwardsHTTPAndConnect(t *testing.T) {
	t.Parallel()
	up := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, "hello "+r.URL.Path)
	}))
	defer up.Close()
	p, err := startProxy("127.0.0.1", "")
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
	p, err := startProxy("127.0.0.1", "s3cret-token")
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
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
		req, _ := http.NewRequest("POST", "http://"+p.Addr()+"/bough/exec", strings.NewReader(`{"args":["update"]}`))
		if auth != "" {
			req.Header.Set("Authorization", auth)
		}
		resp, err := http.DefaultClient.Do(req)
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
	if _, err := exec.LookPath("python3"); err != nil {
		t.Skip("python3 not installed")
	}
	dir := t.TempDir()
	fake := filepath.Join(dir, "host-bough")
	os.WriteFile(fake, []byte("#!/bin/sh\necho ok\n"), 0o755)
	prev := hostBough
	hostBough = func() (string, error) { return fake, nil }
	defer func() { hostBough = prev }()
	p, err := startProxy("127.0.0.1", "tok-12345")
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
