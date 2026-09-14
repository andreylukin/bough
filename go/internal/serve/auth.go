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
// ride it.
const TokenCookie = "bough_serve_token"

// Guard wraps the API with the checks a local server with no login
// needs: a Host that is this machine (DNS rebinding), no cross-origin
// state changes (CSRF), and the token on every /api request. remote
// is true only for an explicit non-loopback bind; it drops the Host
// check, never the token.
func Guard(h http.Handler, token string, remote bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		loop := LoopbackHost(r.Host)
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
				Name: TokenCookie, Value: token, Path: "/",
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
	} else if c, err := r.Cookie(TokenCookie); err == nil {
		got = c.Value
	}
	return token != "" && subtle.ConstantTimeCompare([]byte(got), []byte(token)) == 1
}

// LoopbackHost reports whether a Host header names this machine:
// localhost, 127.0.0.0/8 or [::1], with or without a port.
func LoopbackHost(host string) bool {
	if h, _, err := net.SplitHostPort(host); err == nil {
		host = h
	}
	host = strings.TrimSuffix(strings.TrimPrefix(host, "["), "]")
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
