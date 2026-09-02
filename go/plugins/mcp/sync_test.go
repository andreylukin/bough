package mcp

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	sdk "github.com/modelcontextprotocol/go-sdk/mcp"
)

const fakeCreds = `{"claudeAiOauth":{"accessToken":"never-read"},"mcpOAuth":{
  "linear-server|abc":{"serverName":"linear-server","serverUrl":"https://mcp.linear.app/mcp","accessToken":"tok-linear","expiresAt":4102444800000},
  "plugin:slack:slack|def":{"serverName":"plugin:slack:slack","serverUrl":"https://mcp.slack.com/mcp","accessToken":"tok-slack","expiresAt":1000},
  "nourl|ghi":{"serverName":"nourl","accessToken":"x"}}}`

func TestGrantsAndRender(t *testing.T) {
	var warned []string
	grants, err := grantsOf([]byte(fakeCreds), func(m string) { warned = append(warned, m) })
	if err != nil || len(grants) != 2 || grants[0].Name != "linear-server" || grants[1].Name != "plugin:slack:slack" {
		t.Fatalf("grants = %+v, %v", grants, err)
	}
	if len(warned) != 1 || !strings.Contains(warned[0], "nourl") {
		t.Fatalf("warned = %v", warned)
	}
	if got := rowNames(grants); got[0] != "linear-server" || got[1] != "slack" {
		t.Fatalf("rowNames = %v", got)
	}
	out := render(grants, time.Date(2026, 9, 2, 0, 0, 0, 0, time.UTC))
	if strings.Contains(string(out), "tok-") {
		t.Fatal("a token leaked into the rendered file")
	}
	var doc struct {
		Servers map[string]struct {
			URL      string            `json:"url"`
			Headers  map[string]string `json:"headers"`
			Disabled bool              `json:"disabled"`
		} `json:"servers"`
	}
	if err := json.Unmarshal(out, &doc); err != nil {
		t.Fatal(err)
	}
	lin := doc.Servers["linear-server"]
	if lin.URL != "https://mcp.linear.app/mcp" || lin.Disabled ||
		lin.Headers["Authorization"] != "Bearer ${keychain:Claude Code-credentials#mcpOAuth.linear-server|abc.accessToken}" {
		t.Fatalf("linear entry = %+v", lin)
	}
	if !doc.Servers["slack"].Disabled {
		t.Fatal("an expired grant should be disabled")
	}
	// The rendered file parses back as servers, with the keychain
	// reference resolving through the (stubbed) keychain.
	parsed := parseDoc(t, out)
	if parsed["linear-server"].URL == "" || !parsed["slack"].Disabled {
		t.Fatalf("parsed = %+v", parsed)
	}
	readSecret = func(string) ([]byte, error) { return []byte(fakeCreds), nil }
	t.Cleanup(func() { readSecret = nil })
	if v, err := resolveRef(parsed["linear-server"].Headers["Authorization"]); err != nil || v != "Bearer tok-linear" {
		t.Fatalf("resolveRef = (%q, %v)", v, err)
	}
}

func parseDoc(t *testing.T, data []byte) map[string]ServerConfig {
	t.Helper()
	var doc struct {
		Servers map[string]map[string]any `json:"servers"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		t.Fatal(err)
	}
	out := map[string]ServerConfig{}
	for n, m := range doc.Servers {
		sc, ok := parseEntry(m)
		if !ok {
			t.Fatalf("entry %s did not parse", n)
		}
		out[n] = sc
	}
	return out
}

func TestJSONPath(t *testing.T) {
	data := []byte(`{"a":{"b.c":{"d":"deep"},"b":{"c":{"d":"shallow"}}},"n":5}`)
	if v, _ := jsonPath(data, "a.b.c.d"); v != "deep" {
		t.Fatalf("longest-key match = %q, want deep", v)
	}
	if v, _ := jsonPath(data, "n"); v != "5" {
		t.Fatalf("scalar = %q", v)
	}
	if _, err := jsonPath(data, "a.zz"); err == nil {
		t.Fatal("missing key should error")
	}
}

// An HTTP server entry connects over streamable HTTP with its resolved
// header attached to every request.
func TestConnectHTTPWithResolvedHeader(t *testing.T) {
	srv := sdk.NewServer(&sdk.Implementation{Name: "h", Version: "0"}, nil)
	sdk.AddTool(srv, &sdk.Tool{Name: "ping", Description: "pong"},
		func(context.Context, *sdk.CallToolRequest, map[string]any) (*sdk.CallToolResult, any, error) {
			return &sdk.CallToolResult{Content: []sdk.Content{&sdk.TextContent{Text: "pong"}}}, nil, nil
		})
	handler := sdk.NewStreamableHTTPHandler(func(*http.Request) *sdk.Server { return srv }, nil)
	var seen string
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = r.Header.Get("Authorization")
		handler.ServeHTTP(w, r)
	}))
	defer ts.Close()
	readSecret = func(string) ([]byte, error) { return []byte(fakeCreds), nil }
	t.Cleanup(func() { readSecret = nil })
	sc := ServerConfig{URL: ts.URL, Headers: map[string]string{
		"Authorization": "Bearer ${keychain:Claude Code-credentials#mcpOAuth.linear-server|abc.accessToken}"}}
	session, err := connect(sc)
	if err != nil {
		t.Fatal(err)
	}
	defer session.Close()
	if seen != "Bearer tok-linear" {
		t.Fatalf("Authorization seen by the server = %q", seen)
	}
	out, err := callOn(session, "ping", "")
	if err != nil || out != "pong" {
		t.Fatalf("call over http = (%q, %v)", out, err)
	}
	if _, err := connect(ServerConfig{URL: ts.URL, Disabled: true, Note: "expired"}); err == nil {
		t.Fatal("a disabled server must not connect")
	}
}
