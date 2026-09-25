//go:build unix

package history

import (
	"bufio"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// TestLeaseHelper is not a test: it is the second process
// TestSessionLeaseAcrossProcesses runs (the test binary itself), which
// takes the lease on $LEASE_PATH, says so on stdout and holds it until
// its stdin closes.
func TestLeaseHelper(t *testing.T) {
	path := os.Getenv("LEASE_PATH")
	if path == "" {
		t.Skip("helper process only")
	}
	release, err := TakeLease(path)
	if err != nil {
		os.Stdout.WriteString("error " + err.Error() + "\n")
		return
	}
	defer release()
	os.Stdout.WriteString("held\n")
	bufio.NewReader(os.Stdin).ReadString('\n')
}

// One process holds a session file's lease at a time: a second one is
// refused and told who holds it, LeaseHolder names the holder to
// anyone else, the holder retaking it (a history row remount) is fine,
// and the lease is free again once the holder is gone.
func TestSessionLeaseAcrossProcesses(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "s1.jsonl")
	cmd := exec.Command(os.Args[0], "-test.run=^TestLeaseHelper$")
	cmd.Env = append(os.Environ(), "LEASE_PATH="+path)
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
		t.Fatalf("helper: %q", line)
	}

	if got := LeaseHolder(path); got != cmd.Process.Pid {
		t.Errorf("LeaseHolder = %d, want the helper's pid %d", got, cmd.Process.Pid)
	}
	_, err = TakeLease(path)
	var held *ErrLeased
	if !errors.As(err, &held) || held.Pid != cmd.Process.Pid || held.ID != "s1" {
		t.Fatalf("TakeLease while the helper holds it = %v, want ErrLeased naming pid %d", err, cmd.Process.Pid)
	}
	if !strings.Contains(err.Error(), strconv.Itoa(cmd.Process.Pid)) {
		t.Errorf("error %q does not name the holder", err)
	}

	stdin.Close()
	cmd.Wait()
	if got := LeaseHolder(path); got != 0 {
		t.Errorf("LeaseHolder after the helper exited = %d, want 0", got)
	}
	release, err := TakeLease(path)
	if err != nil {
		t.Fatalf("TakeLease once free: %v", err)
	}
	again, err := TakeLease(path)
	if err != nil {
		t.Fatalf("the holder retaking its own lease: %v", err)
	}
	if got := LeaseHolder(path); got != 0 {
		t.Errorf("LeaseHolder of this process's own lease = %d, want 0", got)
	}
	again()
	release()
}
