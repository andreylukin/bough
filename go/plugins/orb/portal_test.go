package orb

import (
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/kernel"
)

// startingOrb is the "orb" service's readiness half.
type startingOrb struct{ starting bool }

func (o startingOrb) Starting() bool { return o.starting }

// The native portal is registered before the orb is up, refuses while
// it is not, forwards once it is, and leaves with the row.
func TestNativePortal(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	ctx := kernel.NewContext()
	reg := agenttools.NewRegistry()
	ctx.Provide("agent-tools", reg)
	registerPortalTools(ctx, home, "s1")
	tl, ok := reg.Lookup("portal")
	if !ok {
		t.Fatal("native portal not registered before the orb")
	}
	call := func(args string) agenttools.Result {
		t.Helper()
		r, err := tl.Call(context.Background(), agenttools.Call{Args: json.RawMessage(args)})
		if err != nil {
			t.Fatal(err)
		}
		return r
	}
	if r := call(`{"port":3000}`); !strings.Contains(r.Error, "project orb not ready") {
		t.Fatalf("no orb = %+v", r)
	}
	ctx.Provide("orb", startingOrb{starting: true})
	if r := call(`{"port":3000}`); !strings.Contains(r.Error, "project orb not ready") {
		t.Fatalf("starting orb = %+v", r)
	}
	ctx.Provide("orb", startingOrb{})

	guest := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("hello from the orb"))
	}))
	defer guest.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(guest.URL, "http://"))
	st, _ := json.Marshal(iorb.State{Session: "s1", Status: iorb.StatusRunning, IP: host})
	if err := os.MkdirAll(iorb.Dir(home, "s1"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(iorb.Dir(home, "s1"), "state.json"), st, 0o644); err != nil {
		t.Fatal(err)
	}
	r := call(`{"op":"open","port":` + port + `,"label":"web"}`)
	var opened struct {
		URL   string
		Guest int
		Name  string
	}
	if err := json.Unmarshal([]byte(r.Text), &opened); err != nil || r.Error != "" || opened.Name != "web" || strconv.Itoa(opened.Guest) != port {
		t.Fatalf("open = %+v (%v)", r, err)
	}
	res, err := http.Get(opened.URL)
	if err != nil {
		t.Fatal(err)
	}
	b, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if string(b) != "hello from the orb" {
		t.Fatalf("through the portal: %q", b)
	}
	if r := call(`{"op":"list"}`); !strings.Contains(r.Text, `"name":"web"`) {
		t.Fatalf("list = %+v", r)
	}
	if d := tl.Detail(json.RawMessage(`{"port":3000}`)); d != "open 3000" {
		t.Fatalf("detail = %q", d)
	}
	ctx.Unmount()
	if _, ok := reg.Lookup("portal"); ok {
		t.Fatal("unmount left the native portal")
	}
	// A dial, not a GET: the client's kept-alive connection outlives the listener.
	if c, err := net.Dial("tcp", strings.TrimPrefix(opened.URL, "http://")); err == nil {
		c.Close()
		t.Fatal("unmount left the portal listening")
	}
}
