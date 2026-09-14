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
	p, err := startProxy("127.0.0.1")
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
	p, err := startProxy("127.0.0.1")
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
	p, err := startProxy("127.0.0.1")
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

	conn, err := net.Dial("tcp", p.ln.Addr().String())
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
