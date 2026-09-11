package vtreal

import (
	"os/exec"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

// A booted child serves its pages on an ephemeral port, never on the
// user's localhost:7683: one test child holding that port made every
// real bough's artifact URLs 404 or show that build's pages.
func TestChildWebRowNotOnUserPort(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("lsof")
	}
	if _, err := exec.LookPath("lsof"); err != nil {
		t.Skip("lsof not installed")
	}
	t.Parallel()
	a := start(t, 80, 24)
	pid := a.cmd.Process.Pid
	var out string
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(100 * time.Millisecond) {
		b, _ := exec.Command("lsof", "-a", "-p", strconv.Itoa(pid), "-iTCP", "-sTCP:LISTEN", "-P", "-n", "-Fn").Output()
		if out = string(b); strings.Contains(out, "\nn") {
			break
		}
	}
	if strings.Contains(out, ":7683") {
		t.Fatalf("child %d listens on the user's port 7683:\n%s", pid, out)
	}
	if !strings.Contains(out, "\nn127.0.0.1:") {
		t.Fatalf("child %d has no web listener on 127.0.0.1:<ephemeral>:\n%s", pid, out)
	}
}
