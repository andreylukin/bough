package orb

import (
	"strings"
	"testing"
	"time"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

// The model is told the container's address, so it stops pointing the
// user at localhost, and exactly which host forwards exist.
func TestPromptSectionAddress(t *testing.T) {
	t.Parallel()
	st := iorb.State{Project: "web", Container: "bough-orb-s1", Status: iorb.StatusRunning, IP: "192.168.64.7",
		Ports: []iorb.PortState{{Host: 3000, Guest: 3000}, {Host: 5173, Guest: 5173, Error: "127.0.0.1:5173 is in use on the host"}}}
	s := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{})
	for _, want := range []string{"192.168.64.7", "http://192.168.64.7:<port>", "not localhost", "0.0.0.0", "http://127.0.0.1:3000", "5173 is not forwarded: 127.0.0.1:5173 is in use"} {
		if !strings.Contains(s, want) {
			t.Errorf("section lacks %q:\n%s", want, s)
		}
	}
	// No forwards: say how to opt in.
	st.Ports = nil
	if s := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{}); !strings.Contains(s, "bough project set web ports") {
		t.Errorf("no opt-in hint:\n%s", s)
	}
	// No address (runtime could not say): no invented one.
	st.IP = ""
	if s := promptSection("/r", "/p/MEMORY.md", st, projectdef.Def{}, nil, projectRole{}); strings.Contains(s, "http://:") {
		t.Errorf("empty address:\n%s", s)
	}
}

func TestOrbStatusAddress(t *testing.T) {
	t.Parallel()
	st := iorb.State{Project: "web", Status: iorb.StatusRunning, IP: "192.168.64.7",
		Ports: []iorb.PortState{{Host: 8080, Guest: 80}, {Host: 3000, Guest: 3000, Error: "127.0.0.1:3000 is in use on the host"}}}
	s := orbStatus(st, time.Now())
	for _, want := range []string{"ip 192.168.64.7", "127.0.0.1:8080 → 80", "3000 not forwarded: 127.0.0.1:3000 is in use"} {
		if !strings.Contains(s, want) {
			t.Errorf("status lacks %q:\n%s", want, s)
		}
	}
}
