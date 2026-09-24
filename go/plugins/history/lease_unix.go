//go:build unix

package history

import (
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

// A session's lease is an exclusive flock on <dir>/.lease/<id>, held by
// the one process that has <dir>/<id>.jsonl open for its whole life: a
// serve child or a terminal `bough -r`. Append's per-append flock keeps
// lines whole but let two processes run two turns into one file; the
// lease is what tells the second process, and serve, which starts
// children, that the session is taken. The kernel drops it when the
// holder dies, so a crash never strands it. The file holds the
// holder's pid, for the message.

// leaseWait bounds how long a take retries: LeaseHolder's probe holds a
// shared lock for an instant, and a take must not fail on it.
const leaseWait = 500 * time.Millisecond

var leases = struct {
	sync.Mutex
	m map[string]*lease
}{m: map[string]*lease{}}

type lease struct {
	f *os.File
	n int
}

// TakeLease takes the lease on the session file at path for this
// process. A process that already holds it takes it again (a remount of
// the history row); each release gives back one take.
func TakeLease(path string) (release func(), err error) {
	lp := leasePath(path)
	leases.Lock()
	defer leases.Unlock()
	if l, ok := leases.m[lp]; ok {
		l.n++
		return releaser(lp, l), nil
	}
	if err := os.MkdirAll(filepath.Dir(lp), 0o755); err != nil {
		return nil, err
	}
	f, err := os.OpenFile(lp, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, err
	}
	for deadline := time.Now().Add(leaseWait); ; {
		err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			break
		}
		if !errors.Is(err, syscall.EWOULDBLOCK) && !errors.Is(err, syscall.EINTR) {
			f.Close()
			return nil, err
		}
		if time.Now().After(deadline) {
			pid := readPid(f)
			f.Close()
			return nil, &ErrLeased{ID: strings.TrimSuffix(filepath.Base(path), ".jsonl"), Pid: pid}
		}
		time.Sleep(10 * time.Millisecond)
	}
	f.Truncate(0)
	f.WriteAt([]byte(strconv.Itoa(os.Getpid())), 0)
	l := &lease{f: f, n: 1}
	leases.m[lp] = l
	return releaser(lp, l), nil
}

func releaser(lp string, l *lease) func() {
	var once sync.Once
	return func() {
		once.Do(func() {
			leases.Lock()
			defer leases.Unlock()
			if l.n--; l.n == 0 {
				delete(leases.m, lp)
				syscall.Flock(int(l.f.Fd()), syscall.LOCK_UN)
				l.f.Close()
			}
		})
	}
}

func readPid(f *os.File) int {
	b := make([]byte, 32)
	n, _ := f.ReadAt(b, 0)
	pid, _ := strconv.Atoi(strings.TrimSpace(string(b[:n])))
	return pid
}

// LeaseHolder is the pid of another process holding the lease on the
// session file at path, 0 when none does (or only this one), -1 when
// one does and its pid is unreadable.
func LeaseHolder(path string) int {
	lp := leasePath(path)
	leases.Lock()
	_, mine := leases.m[lp]
	leases.Unlock()
	if mine {
		return 0
	}
	f, err := os.Open(lp)
	if err != nil {
		return 0
	}
	defer f.Close()
	if syscall.Flock(int(f.Fd()), syscall.LOCK_SH|syscall.LOCK_NB) == nil {
		syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
		return 0
	}
	if pid := readPid(f); pid > 0 {
		return pid
	}
	return -1
}
