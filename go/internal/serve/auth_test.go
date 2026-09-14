package serve

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func guardRig(t *testing.T) http.Handler {
	t.Helper()
	ok := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(http.StatusOK) })
	return Guard(ok, "sekret", false)
}

func guardDo(h http.Handler, method, path, host string, hdr map[string]string) *httptest.ResponseRecorder {
	req := httptest.NewRequest(method, "http://"+host+path, strings.NewReader(""))
	req.Host = host
	for k, v := range hdr {
		req.Header.Set(k, v)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	return rec
}

func TestGuardForeignHostRejected(t *testing.T) {
	t.Parallel()
	h := guardRig(t)
	for _, host := range []string{"evil.example:7684", "10.0.0.5:7684", "localhost.evil.example"} {
		if rec := guardDo(h, "GET", "/api/health", host, map[string]string{"Authorization": "Bearer sekret"}); rec.Code != http.StatusForbidden {
			t.Errorf("host %s = %d, want 403", host, rec.Code)
		}
	}
	for _, host := range []string{"127.0.0.1:7684", "localhost:7684", "[::1]:7684", "localhost"} {
		if rec := guardDo(h, "GET", "/api/health", host, map[string]string{"Authorization": "Bearer sekret"}); rec.Code != http.StatusOK {
			t.Errorf("host %s = %d, want 200", host, rec.Code)
		}
	}
	remote := Guard(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}), "sekret", true)
	if rec := guardDo(remote, "GET", "/api/health", "10.0.0.5:7684", map[string]string{"Authorization": "Bearer sekret"}); rec.Code != http.StatusOK {
		t.Errorf("remote bind = %d, want 200", rec.Code)
	}
}

func TestGuardCrossOriginPostRejected(t *testing.T) {
	t.Parallel()
	h := guardRig(t)
	auth := "Bearer sekret"
	if rec := guardDo(h, "PUT", "/api/hooks/file", "127.0.0.1:7684", map[string]string{"Authorization": auth, "Origin": "http://evil.example"}); rec.Code != http.StatusForbidden {
		t.Errorf("cross-origin PUT = %d, want 403", rec.Code)
	}
	if rec := guardDo(h, "POST", "/api/sessions", "127.0.0.1:7684", map[string]string{"Authorization": auth, "Origin": "http://localhost:7683"}); rec.Code != http.StatusForbidden {
		t.Errorf("other-port POST = %d, want 403", rec.Code)
	}
	if rec := guardDo(h, "POST", "/api/sessions", "127.0.0.1:7684", map[string]string{"Authorization": auth, "Origin": "http://127.0.0.1:7684"}); rec.Code != http.StatusOK {
		t.Errorf("same-origin POST = %d, want 200", rec.Code)
	}
}

func TestGuardToken(t *testing.T) {
	t.Parallel()
	h := guardRig(t)
	host := "127.0.0.1:7684"
	if rec := guardDo(h, "GET", "/api/sessions", host, nil); rec.Code != http.StatusUnauthorized {
		t.Errorf("no token = %d, want 401", rec.Code)
	}
	if rec := guardDo(h, "GET", "/api/sessions", host, map[string]string{"Authorization": "Bearer wrong"}); rec.Code != http.StatusUnauthorized {
		t.Errorf("wrong token = %d, want 401", rec.Code)
	}
	if rec := guardDo(h, "GET", "/api/sessions", host, map[string]string{"Authorization": "Bearer sekret"}); rec.Code != http.StatusOK {
		t.Errorf("bearer = %d, want 200", rec.Code)
	}
	if rec := guardDo(h, "GET", "/api/sessions", host, map[string]string{"Cookie": TokenCookie + "=sekret"}); rec.Code != http.StatusOK {
		t.Errorf("cookie = %d, want 200", rec.Code)
	}
	// The page sets the cookie, and does not need the token itself.
	rec := guardDo(h, "GET", "/", host, nil)
	c := rec.Result().Cookies()
	if rec.Code != http.StatusOK || len(c) != 1 || c[0].Value != "sekret" || !c[0].HttpOnly || c[0].SameSite != http.SameSiteStrictMode {
		t.Errorf("page = %d cookies %+v", rec.Code, c)
	}
}
