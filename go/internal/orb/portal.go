package orb

import (
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"sync"
	"time"
)

// A portal is a way to look at a web server the agent started inside an
// orb. The container's own IP already reaches every guest port from the
// host, so a portal adds no reachability; what it adds is a loopback
// address. The bridge is shared by every orb and VM on the machine, so
// linking the browser straight at the container IP would put the user's
// dev server in front of every other orb too. Published ports (ports.go)
// are 127.0.0.1-only for that reason, and a portal keeps the same
// promise — but unlike them it can be opened on a container that is
// already running, which is what makes it useful to an agent mid-turn.
//
// A portal is a route to a process, not a deployment. It belongs to the
// session that opened it and dies with it; the listener lives in that
// process and state.json records it so the control room can show a link.

// PortalState is one open portal, as state.json records it.
type PortalState struct {
	Guest int    `json:"guest"`          // the port the server listens on inside the orb
	Host  int    `json:"host"`           // the loopback port that reaches it
	Name  string `json:"name,omitempty"` // what the agent called it, for the tab
}

// URL is where a browser on this machine opens the portal.
func (p PortalState) URL() string { return "http://127.0.0.1:" + strconv.Itoa(p.Host) }

// portals are the listeners this process owns, keyed by session and
// guest port. Only the session that opened a portal holds its listener.
var portals struct {
	sync.Mutex
	open map[string]map[int]*portal
}

type portal struct {
	ln   net.Listener
	dial string // container IP:guest, resolved when the portal opened
}

// OpenPortal forwards 127.0.0.1:<free port> to <orb IP>:guest for the
// session's running orb. Opening a portal that is already open returns
// the one already there, so an agent asking twice gets one listener and
// the same URL.
func OpenPortal(home, session string, guest int, name string) (PortalState, error) {
	if guest < 1 || guest > 65535 {
		return PortalState{}, fmt.Errorf("orb: portal: port %d is not a port", guest)
	}
	st, err := ReadState(home, session)
	if err != nil || st.Session == "" {
		return PortalState{}, fmt.Errorf("orb: portal: session %s has no orb", session)
	}
	if st.Status != StatusRunning {
		return PortalState{}, fmt.Errorf("orb: portal: the orb is %s, so nothing inside it is listening", statusWord(st.Status))
	}
	if st.IP == "" {
		return PortalState{}, errors.New("orb: portal: the orb has no address yet; it may still be starting")
	}

	portals.Lock()
	defer portals.Unlock()
	if p, ok := portals.open[session][guest]; ok {
		return PortalState{Guest: guest, Host: hostPort(p.ln), Name: name}, nil
	}

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return PortalState{}, fmt.Errorf("orb: portal: %w", err)
	}
	p := &portal{ln: ln, dial: net.JoinHostPort(st.IP, strconv.Itoa(guest))}
	if portals.open == nil {
		portals.open = map[string]map[int]*portal{}
	}
	if portals.open[session] == nil {
		portals.open[session] = map[int]*portal{}
	}
	portals.open[session][guest] = p
	go p.serve()

	ps := PortalState{Guest: guest, Host: hostPort(ln), Name: name}
	if err := recordPortal(home, session, ps); err != nil {
		return ps, err
	}
	return ps, nil
}

// ClosePortal stops the listener and forgets it. Connections already
// open are left to finish; closing the listener only stops new ones.
func ClosePortal(home, session string, guest int) error {
	portals.Lock()
	p := portals.open[session][guest]
	if p != nil {
		delete(portals.open[session], guest)
		if len(portals.open[session]) == 0 {
			delete(portals.open, session)
		}
	}
	portals.Unlock()
	if p != nil {
		p.ln.Close()
	}
	return forgetPortal(home, session, guest)
}

// CloseSessionPortals drops every portal of a session, for its exit.
func CloseSessionPortals(home, session string) {
	portals.Lock()
	open := portals.open[session]
	delete(portals.open, session)
	portals.Unlock()
	for _, p := range open {
		p.ln.Close()
	}
	if len(open) > 0 {
		if st, err := ReadState(home, session); err == nil && st.Session != "" {
			st.Portals = nil
			writeState(home, st)
		}
	}
}

// serve copies bytes both ways. A portal is deliberately a TCP forward
// and not an HTTP proxy: a dev server's absolute asset paths, its
// websocket for live reloading and its cookies all pass through
// untouched, and there is no URL to rewrite.
func (p *portal) serve() {
	for {
		in, err := p.ln.Accept()
		if err != nil {
			return // the listener closed
		}
		go p.pipe(in)
	}
}

func (p *portal) pipe(in net.Conn) {
	defer in.Close()
	out, err := net.DialTimeout("tcp", p.dial, 5*time.Second)
	if err != nil {
		return // nothing listening inside the orb yet
	}
	defer out.Close()
	done := make(chan struct{}, 2)
	cp := func(dst, src net.Conn) {
		io.Copy(dst, src)
		// Let the other side see EOF instead of waiting out a timeout.
		if c, ok := dst.(*net.TCPConn); ok {
			c.CloseWrite()
		}
		done <- struct{}{}
	}
	go cp(out, in)
	go cp(in, out)
	<-done
	<-done
}

func hostPort(ln net.Listener) int { return ln.Addr().(*net.TCPAddr).Port }

// recordPortal adds one portal to state.json, replacing any entry for
// the same guest port.
func recordPortal(home, session string, ps PortalState) error {
	st, err := ReadState(home, session)
	if err != nil || st.Session == "" {
		return nil
	}
	next := make([]PortalState, 0, len(st.Portals)+1)
	for _, p := range st.Portals {
		if p.Guest != ps.Guest {
			next = append(next, p)
		}
	}
	st.Portals = append(next, ps)
	return writeState(home, st)
}

func forgetPortal(home, session string, guest int) error {
	st, err := ReadState(home, session)
	if err != nil || st.Session == "" {
		return nil
	}
	next := make([]PortalState, 0, len(st.Portals))
	for _, p := range st.Portals {
		if p.Guest != guest {
			next = append(next, p)
		}
	}
	st.Portals = next
	return writeState(home, st)
}

// statusWord says an orb's state the way the user reads it elsewhere.
func statusWord(s Status) string {
	switch s {
	case StatusNone:
		return "not a project session"
	case StatusRunning:
		return "running"
	case StatusStopped:
		return "stopped"
	case StatusFailed:
		return "failed to start"
	case StatusBuilding:
		return "still building"
	case StatusStarting:
		return "still starting"
	}
	return string(s)
}
