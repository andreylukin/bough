//go:build !windows

package serve

import (
	"bufio"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// TestLeaseHolderHelper is not a test: it is the terminal `bough -r`
// TestAPISessionHeldElsewhere runs beside serve (the test binary
// itself), holding the lease on $SERVE_TEST_LEASE until stdin closes.
func TestLeaseHolderHelper(t *testing.T) {
	path := os.Getenv("SERVE_TEST_LEASE")
	if path == "" {
		t.Skip("helper process only")
	}
	release, err := history.TakeLease(path)
	if err != nil {
		os.Stdout.WriteString("error " + err.Error() + "\n")
		return
	}
	defer release()
	os.Stdout.WriteString("held\n")
	bufio.NewReader(os.Stdin).ReadString('\n')
}

// holdLease runs the helper on session id's file and returns when it
// holds the lease; closing the returned func lets it exit.
func holdLease(t *testing.T, f *apiFixture, id string) func() {
	t.Helper()
	cmd := exec.Command(os.Args[0], "-test.run=^TestLeaseHolderHelper$")
	cmd.Env = append(os.Environ(), "SERVE_TEST_LEASE="+filepath.Join(f.hist, id+".jsonl"))
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	line, _ := bufio.NewReader(stdout).ReadString('\n')
	if strings.TrimSpace(line) != "held" {
		t.Fatalf("lease helper: %q", line)
	}
	done := func() { stdin.Close(); cmd.Wait() }
	t.Cleanup(done)
	return done
}

// A turn open in a process serve did not start (a terminal `bough -r`)
// is running elsewhere, not interrupted: serve offers no Send on it and
// refuses one, which would start a second writer beside it. Once that
// process is gone the dangling turn is interrupted, and Send resumes it.
func TestAPISessionHeldElsewhere(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	now := time.Now()
	f.seed(t, "held",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "in a terminal"}},
	)
	release := holdLease(t, f, "held")

	code, body := f.do(t, "GET", "/api/sessions/held", "")
	if code != http.StatusOK {
		t.Fatalf("get = %d %v", code, body)
	}
	if got := rowOf(t, body)["status"]; got != string(StatusElsewhere) {
		t.Errorf("status of a turn a terminal is running = %v, want %s", got, StatusElsewhere)
	}
	code, body = f.do(t, "POST", "/api/sessions/held/prompt", `{"text":"from the web"}`)
	if code != http.StatusConflict {
		t.Errorf("prompt while a terminal holds the session = %d %v, want 409", code, body)
	}
	if f.sup.Live("held") || f.startCount(t) != 0 {
		t.Fatal("serve started a child on a session another process holds")
	}

	release()
	code, body = f.do(t, "GET", "/api/sessions/held", "")
	if got := rowOf(t, body)["status"]; code != http.StatusOK || got != string(StatusInterrupted) {
		t.Errorf("status once the terminal is gone = %d %v, want interrupted", code, got)
	}
}
