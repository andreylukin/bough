package orb

import (
	"context"
	"crypto/subtle"
	"encoding/base64"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"time"
)

// proxy is the orb's way out through the host. Container traffic leaves
// the Mac from a VM bridge, which per-app VPNs
// do not tunnel, so internal hosts the user reaches are unreachable from
// the guest. Pointing HTTPS_PROXY at this listener makes every connection
// originate from the host process, with the user's network access.
//
// The listener is on the VM bridge, which every orb shares, so a token
// (the orb's, see token.go) is required: as proxy credentials in the URL
// for proxied traffic, as a bearer token for the relay. An empty token is
// an orb created before tokens: open, as it always was.
type proxy struct {
	ln    net.Listener
	srv   *http.Server
	token string
	bin   func() (string, error) // the host bough relayed calls run
}

// startProxy listens on addr (the guest's gateway IP, so only the host and
// its VMs can reach it) on a free port.
func startProxy(addr, token string) (*proxy, error) {
	return startProxyBin(addr, token, os.Executable)
}

// startProxyBin is startProxy relaying to bin's binary. Tests hand a fake
// host bough in here rather than swapping a package var, so they can run
// in parallel.
func startProxyBin(addr, token string, bin func() (string, error)) (*proxy, error) {
	ln, err := net.Listen("tcp", net.JoinHostPort(addr, "0"))
	if err != nil {
		return nil, err
	}
	p := &proxy{ln: ln, token: token, bin: bin}
	p.srv = &http.Server{Handler: http.HandlerFunc(p.serve), ReadHeaderTimeout: 30 * time.Second}
	go p.srv.Serve(ln)
	return p, nil
}

// Addr is host:port, with no credentials.
func (p *proxy) Addr() string { return p.ln.Addr().String() }

// URL carries the token as proxy credentials, which curl, git, Go,
// Python and Node send as Proxy-Authorization.
func (p *proxy) URL() string {
	if p.token == "" {
		return "http://" + p.Addr()
	}
	return "http://" + proxyUser + ":" + p.token + "@" + p.Addr()
}

const proxyUser = "bough"

// relayDenied is the shim's hint when the relay refuses it.
const relayDenied = "orb relay: missing or wrong orb token (an orb created before proxy tokens has none: remove the orb and start a session to recreate it)"

func (p *proxy) tokenOK(got string) bool {
	return subtle.ConstantTimeCompare([]byte(got), []byte(p.token)) == 1
}

// proxyAuthorized checks Proxy-Authorization: Basic bough:<token>.
func (p *proxy) proxyAuthorized(r *http.Request) bool {
	if p.token == "" {
		return true
	}
	h, ok := strings.CutPrefix(r.Header.Get("Proxy-Authorization"), "Basic ")
	if !ok {
		return false
	}
	b, err := base64.StdEncoding.DecodeString(strings.TrimSpace(h))
	if err != nil {
		return false
	}
	user, pass, _ := strings.Cut(string(b), ":")
	return user == proxyUser && p.tokenOK(pass)
}

// relayAuthorized checks Authorization: Bearer <token>.
func (p *proxy) relayAuthorized(r *http.Request) bool {
	if p.token == "" {
		return true
	}
	h, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
	return ok && p.tokenOK(strings.TrimSpace(h))
}

func (p *proxy) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	return p.srv.Shutdown(ctx)
}

var transport = &http.Transport{Proxy: nil, ForceAttemptHTTP2: false, IdleConnTimeout: 90 * time.Second}

func (p *proxy) serve(w http.ResponseWriter, r *http.Request) {
	if r.URL.Host == "" && r.Method == http.MethodPost && r.URL.Path == "/bough/exec" {
		if !p.relayAuthorized(r) {
			http.Error(w, relayDenied, http.StatusUnauthorized)
			return
		}
		p.relayExec(w, r)
		return
	}
	if !p.proxyAuthorized(r) {
		w.Header().Set("Proxy-Authenticate", `Basic realm="bough orb"`)
		http.Error(w, "orb proxy: orb token required", http.StatusProxyAuthRequired)
		return
	}
	if r.Method == http.MethodConnect {
		p.tunnel(w, r)
		return
	}
	// Plain HTTP through a proxy arrives with an absolute URI.
	if r.URL.Host == "" {
		http.Error(w, "orb proxy: absolute URI or CONNECT required", http.StatusBadRequest)
		return
	}
	out := r.Clone(r.Context())
	out.RequestURI = ""
	out.Header.Del("Proxy-Connection")
	out.Header.Del("Proxy-Authorization")
	resp, err := transport.RoundTrip(out)
	if err != nil {
		http.Error(w, "orb proxy: "+err.Error(), http.StatusBadGateway)
		return
	}
	defer resp.Body.Close()
	for k, vs := range resp.Header {
		for _, v := range vs {
			w.Header().Add(k, v)
		}
	}
	w.WriteHeader(resp.StatusCode)
	io.Copy(w, resp.Body)
}

func (p *proxy) tunnel(w http.ResponseWriter, r *http.Request) {
	dst, err := net.DialTimeout("tcp", r.Host, 30*time.Second)
	if err != nil {
		http.Error(w, "orb proxy: "+err.Error(), http.StatusBadGateway)
		return
	}
	hj, ok := w.(http.Hijacker)
	if !ok {
		dst.Close()
		http.Error(w, "orb proxy: hijack unsupported", http.StatusInternalServerError)
		return
	}
	src, buf, err := hj.Hijack()
	if err != nil {
		dst.Close()
		return
	}
	src.Write([]byte("HTTP/1.1 200 Connection established\r\n\r\n"))
	// Bytes the client sent after the CONNECT header belong to the tunnel.
	if n := buf.Reader.Buffered(); n > 0 {
		b, _ := buf.Reader.Peek(n)
		dst.Write(b)
	}
	done := make(chan struct{}, 2)
	pipe := func(a, b net.Conn) {
		io.Copy(a, b)
		if c, ok := a.(interface{ CloseWrite() error }); ok {
			c.CloseWrite()
		}
		done <- struct{}{}
	}
	go pipe(dst, src)
	go pipe(src, dst)
	<-done
	<-done
	src.Close()
	dst.Close()
}

// proxyEnv points every common client at the proxy; loopback stays direct.
func proxyEnv(url string) []string {
	var env []string
	for _, k := range []string{"HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"} {
		env = append(env, k+"="+url)
	}
	noProxy := strings.Join([]string{"localhost", "127.0.0.1", "::1"}, ",")
	return append(env, "NO_PROXY="+noProxy, "no_proxy="+noProxy)
}
