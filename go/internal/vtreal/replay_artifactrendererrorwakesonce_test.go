package vtreal

// Surface "artifact-render-error-wakes-once": the renderer's error
// report on a published page (a program the server-side parser
// accepts but the browser renderer cannot draw) must wake the idle
// agent exactly once — no wake loop while the report stays on disk —
// and quitting must release the web row's port. Offline: the web row
// binds a reserved loopback port, the browser is a stub opener, and
// the renderer's POST to /errors is sent by the test.
//
// Counting is deterministic: every tape turn after the publish runs a
// real js block that prints a numbered WOKE marker, so each wake the
// model receives shows up once on stdout.

import (
	"fmt"
	"net"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// artifactRenderErrorWakesOnceProgram parses (known statement shape)
// but names a component the renderer does not have.
const artifactRenderErrorWakesOnceProgram = `root = Card([head, bad])
head = CardHeader("Broken page", "render error")
bad = NoSuchWidget("x")`

func TestArtifactRenderErrorWakesOnce(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	_ = ln.Close()

	publish := jsBlock(fmt.Sprintf("console.log(tools.artifact(\"smoke\", %q))", artifactRenderErrorWakesOnceProgram))
	turns := [][]string{{publish, stopReply("Published.")}}
	for i := 1; i <= 3; i++ { // spare turns: a second wake would print WOKE-2
		turns = append(turns, []string{jsBlock(fmt.Sprintf(`console.log("WOKE-%d")`, i)), stopReply("noted")})
	}
	tape := e2eTape(t, turns...)
	p := startHeadless(t, home, e2eConfig(t, tape, addr), filepath.Join(home, "opened.log"))

	p.send("publish the page")
	out := p.waitDone(1)
	m := artifactURLRe.FindStringSubmatch(out)
	if m == nil {
		t.Fatalf("no artifact URL in the publish result:\n%s", p.screen())
	}
	url := m[0]

	// What the renderer posts when it cannot draw the program.
	body := `{"version":1,"errors":[{"statementId":"bad","message":"unknown component NoSuchWidget","hint":"use a component from tools.artifactGuide()"}]}`
	r, err := http.Post(url+"/errors", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	r.Body.Close()
	if r.StatusCode != 200 {
		t.Fatalf("errors POST = %d", r.StatusCode)
	}

	out = p.waitDone(2)
	if !strings.Contains(out, "WOKE-1") {
		t.Fatalf("the renderer error did not wake the agent:\n%s", p.screen())
	}
	// The report stays on disk; the watcher sweeps every 2 s. Across 3 s
	// (and one more sweep of margin) no second wake may arrive.
	time.Sleep(5 * time.Second)
	out = p.stdout()
	if n := strings.Count(out, "[result] WOKE-"); n != 1 {
		t.Fatalf("renderer error woke the agent %d times, want 1:\n%s", n, p.screen())
	}
	if n := strings.Count(out, "[job] [artifact smoke] the page has 1 error(s)"); n != 1 {
		t.Fatalf("%d error notices delivered, want 1:\n%s", n, p.screen())
	}
	if n := strings.Count("\n"+out, "\n[done]"); n != 2 {
		t.Fatalf("%d turns ran, want 2 (publish + one wake):\n%s", n, p.screen())
	}

	// Quit: closing stdin ends headless; the port must be free after.
	_ = p.stdin.Close()
	deadline := time.Now().Add(15 * time.Second)
	for {
		c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond)
		if err != nil {
			break
		}
		c.Close()
		if time.Now().After(deadline) {
			t.Fatalf("web listener %s still accepting after quit:\n%s", addr, p.screen())
		}
		time.Sleep(100 * time.Millisecond)
	}
}
