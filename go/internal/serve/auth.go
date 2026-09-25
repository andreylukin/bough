package serve

import (
	"crypto/subtle"
	"errors"
	"net"
	"net/http"
	"net/url"
	"strings"
)

// TokenCookie carries the per-install token for the UI. HttpOnly so
// page script never sees it; SameSite=Strict so another site cannot
// ride it. The cookie a serve sets is CookieName of the port it listens
// on; the bare name is still read (a tab an older build signed in, and
// test helpers that sign in without loading "/").
const TokenCookie = "bough_serve_token"

// CookieName is the token cookie of the serve listening on port. A
// browser does not scope cookies by port, so under one name a second
// serve on the same host (a fixture-HOME preview) replaced the first
// tab's cookie and every /api call of that tab answered 401.
func CookieName(port string) string { return TokenCookie + "_" + port }

// cookieName is the token cookie of the serve r reached: named for the
// port it was accepted on, which is serve's own behind a proxy too.
func cookieName(r *http.Request) string {
	if a, ok := r.Context().Value(http.LocalAddrContextKey).(net.Addr); ok {
		if _, port, err := net.SplitHostPort(a.String()); err == nil {
			return CookieName(port)
		}
	}
	return TokenCookie
}

// Guard wraps the API with the checks a local server with no login
// needs: a Host that is this machine (DNS rebinding), no cross-origin
// state changes (CSRF), and the token on every /api request. remote
// is true only for an explicit non-loopback bind; it drops the Host
// check, never the token. host, when set, is one more name this
// machine answers to — the name a reverse proxy in front of a
// loopback bind (tailscale serve) forwards — trusted exactly like
// loopback, cookie included.
func Guard(h http.Handler, token string, remote bool, host string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Guard answers refusals itself, before the API can stamp them,
		// and a page has to tell a restart onto a new build from a
		// server that went away — so a refusal names its build too.
		w.Header().Set(BuildHeader, buildID())
		loop := LoopbackHost(r.Host) || (host != "" && strings.EqualFold(hostOnly(r.Host), host))
		if !remote && !loop {
			writeErr(w, http.StatusForbidden, errors.New("serve: host "+r.Host+" is not loopback"))
			return
		}
		if CrossOrigin(r) {
			writeErr(w, http.StatusForbidden, errors.New("serve: cross-origin request refused"))
			return
		}
		if strings.HasPrefix(r.URL.Path, "/api/") && !hasToken(r, token) {
			writeErr(w, http.StatusUnauthorized, errors.New("serve: missing or wrong token"))
			return
		}
		if r.URL.Path == "/" && loop && (r.Method == http.MethodGet || r.Method == http.MethodHead) {
			http.SetCookie(w, &http.Cookie{
				Name: cookieName(r), Value: token, Path: "/",
				HttpOnly: true, SameSite: http.SameSiteStrictMode,
			})
		}
		h.ServeHTTP(w, r)
	})
}

func hasToken(r *http.Request, token string) bool {
	got := ""
	if a := r.Header.Get("Authorization"); strings.HasPrefix(a, "Bearer ") {
		got = strings.TrimPrefix(a, "Bearer ")
	} else if c, err := r.Cookie(cookieName(r)); err == nil {
		got = c.Value
	} else if c, err := r.Cookie(TokenCookie); err == nil {
		got = c.Value
	}
	return token != "" && subtle.ConstantTimeCompare([]byte(got), []byte(token)) == 1
}

// hostOnly strips the port and IPv6 brackets from a Host header.
func hostOnly(host string) string {
	if h, _, err := net.SplitHostPort(host); err == nil {
		host = h
	}
	return strings.TrimSuffix(strings.TrimPrefix(host, "["), "]")
}

// LoopbackHost reports whether a Host header names this machine:
// localhost, 127.0.0.0/8 or [::1], with or without a port.
func LoopbackHost(host string) bool {
	host = hostOnly(host)
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

// CrossOrigin reports a state-changing request whose Origin is present
// and is not the host it was sent to. Reads are left alone: without
// CORS headers a foreign page cannot read the answer.
func CrossOrigin(r *http.Request) bool {
	switch r.Method {
	case http.MethodGet, http.MethodHead, http.MethodOptions:
		return false
	}
	o := r.Header.Get("Origin")
	if o == "" {
		return false
	}
	u, err := url.Parse(o)
	return err != nil || u.Host == "" || !strings.EqualFold(u.Host, r.Host)
}
