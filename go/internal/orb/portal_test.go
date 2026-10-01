package orb

import (
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

// running writes a state.json whose IP is the loopback host the fake
// "guest" server listens on, which is what a portal dials.
func running(t *testing.T, home, session, ip string) {
	t.Helper()
	if err := writeState(home, State{Session: session, Status: StatusRunning, IP: ip}); err != nil {
		t.Fatal(err)
	}
}

func TestPortalForwards(t *testing.T) {
	home := t.TempDir()
	guest := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("hello from " + r.URL.Path))
	}))
	defer guest.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(guest.URL, "http://"))
	gp, _ := strconv.Atoi(port)
	running(t, home, "s1", host)

	ps, err := OpenPortal(home, "s1", gp, "web")
	if err != nil {
		t.Fatal(err)
	}
	defer ClosePortal(home, "s1", gp)

	res, err := http.Get(ps.URL() + "/thing")
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	b, _ := io.ReadAll(res.Body)
	if got := string(b); got != "hello from /thing" {
		t.Fatalf("through the portal: %q", got)
	}

	// The portal is in state.json, so the control room can link to it.
	st, err := ReadState(home, "s1")
	if err != nil {
		t.Fatal(err)
	}
	if len(st.Portals) != 1 || st.Portals[0].Guest != gp || st.Portals[0].Host != ps.Host {
		t.Fatalf("state.json portals: %+v", st.Portals)
	}
}

// Asking twice is one listener and one URL: an agent that re-opens a
// portal mid-turn should not leak a second forward.
func TestPortalReopenIsTheSameOne(t *testing.T) {
	home := t.TempDir()
	guest := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}))
	defer guest.Close()
	host, port, _ := net.SplitHostPort(strings.TrimPrefix(guest.URL, "http://"))
	gp, _ := strconv.Atoi(port)
	running(t, home, "s1", host)

	first, err := OpenPortal(home, "s1", gp, "web")
	if err != nil {
		t.Fatal(err)
	}
	defer ClosePortal(home, "s1", gp)
	again, err := OpenPortal(home, "s1", gp, "web")
	if err != nil {
		t.Fatal(err)
	}
	if first.Host != again.Host {
		t.Fatalf("reopen moved the portal: %d then %d", first.Host, again.Host)
	}
	if st, _ := ReadState(home, "s1"); len(st.Portals) != 1 {
		t.Fatalf("reopen recorded twice: %+v", st.Portals)
	}
}

func TestClosePortalStopsListening(t *testing.T) {
	t.Parallel()
	testPortalStopsListening(t, ClosePortal)
}

func TestCloseSessionPortalsStopsListening(t *testing.T) {
	t.Parallel()
	testPortalStopsListening(t, func(home, session string, _ int) error {
		CloseSessionPortals(home, session)
		return nil
	})
}

func testPortalStopsListening(t *testing.T, closePortal func(string, string, int) error) {
	t.Helper()
	home, session := t.TempDir(), t.Name()
	const guest = 3000
	running(t, home, session, "127.0.0.1")
	if _, err := OpenPortal(home, session, guest, ""); err != nil {
		t.Fatal(err)
	}
	portals.Lock()
	original := portals.open[session][guest].ln
	portals.Unlock()
	t.Cleanup(func() { CloseSessionPortals(home, session) })
	t.Cleanup(func() { original.Close() })
	if err := closePortal(home, session, guest); err != nil {
		t.Fatal(err)
	}

	// A successful dial to a released port says nothing about its former
	// listener: another test process can already own the address.
	reused, err := net.Listen("tcp", original.Addr().String())
	if err == nil {
		t.Cleanup(func() { reused.Close() })
		c, err := net.DialTimeout("tcp", reused.Addr().String(), 5*time.Second)
		if err != nil {
			t.Fatal(err)
		}
		c.Close()
	} else if !errors.Is(err, syscall.EADDRINUSE) {
		t.Fatal(err)
	}

	// Bound the failure if Close regresses, without sleeping or probing
	// whoever owns the old port now.
	if err := original.(*net.TCPListener).SetDeadline(time.Now()); err != nil && !errors.Is(err, net.ErrClosed) {
		t.Fatal(err)
	}
	if c, err := original.Accept(); !errors.Is(err, net.ErrClosed) {
		if c != nil {
			c.Close()
		}
		t.Fatalf("original portal listener after Close: %v, want net.ErrClosed", err)
	}
	st, err := ReadState(home, session)
	if err != nil {
		t.Fatal(err)
	}
	if len(st.Portals) != 0 {
		t.Fatalf("closed portal still in state.json: %+v", st.Portals)
	}
}

// A portal is only honest about a running orb: a stopped one has nothing
// listening, and saying otherwise hands the user a dead link.
func TestPortalRefusesStoppedOrb(t *testing.T) {
	home := t.TempDir()
	if err := writeState(home, State{Session: "s1", Status: StatusStopped, IP: "127.0.0.1"}); err != nil {
		t.Fatal(err)
	}
	if _, err := OpenPortal(home, "s1", 3000, ""); err == nil {
		t.Fatal("opened a portal on a stopped orb")
	} else if !strings.Contains(err.Error(), "stopped") {
		t.Fatalf("unhelpful error: %v", err)
	}
}

func TestPortalRefusesOrbWithNoAddress(t *testing.T) {
	home := t.TempDir()
	running(t, home, "s1", "")
	if _, err := OpenPortal(home, "s1", 3000, ""); err == nil {
		t.Fatal("opened a portal on an orb with no IP")
	}
}
