package orb

import (
	"context"
	"io"
	"net"
	"net/http"
	"strings"
	"time"
)

// proxy is the orb's way out through the host. Container traffic leaves
// the Mac from a VM bridge, which per-app VPNs
// do not tunnel, so internal hosts the user reaches are unreachable from
// the guest. Pointing HTTPS_PROXY at this listener makes every connection
// originate from the host process, with the user's network access.
type proxy struct {
	ln  net.Listener
	srv *http.Server
}

// startProxy listens on addr (the guest's gateway IP, so only the host and
// its VMs can reach it) on a free port.
func startProxy(addr string) (*proxy, error) {
	ln, err := net.Listen("tcp", net.JoinHostPort(addr, "0"))
	if err != nil {
		return nil, err
	}
	p := &proxy{ln: ln}
	p.srv = &http.Server{Handler: http.HandlerFunc(p.serve), ReadHeaderTimeout: 30 * time.Second}
	go p.srv.Serve(ln)
	return p, nil
}

func (p *proxy) URL() string { return "http://" + p.ln.Addr().String() }

func (p *proxy) Close() error {
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	return p.srv.Shutdown(ctx)
}

var transport = &http.Transport{Proxy: nil, ForceAttemptHTTP2: false, IdleConnTimeout: 90 * time.Second}

func (p *proxy) serve(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodConnect {
		p.tunnel(w, r)
		return
	}
	if r.URL.Host == "" && r.Method == http.MethodPost && r.URL.Path == "/bough/exec" {
		relayExec(w, r)
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
