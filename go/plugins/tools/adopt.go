package tools

import (
	"fmt"
	"sync"
	"time"
)

// callOut is a native bash call's live output tail. The engine may adopt
// a call that outlived its turn as a job (Adopt); jobs and job then read
// what the command has printed so far from here, as they would a
// background job's buffer.
type callOut struct {
	mu  sync.Mutex
	buf []byte
	cut int // bytes dropped from the front
}

func (o *callOut) Write(p []byte) (int, error) {
	o.mu.Lock()
	o.buf = append(o.buf, p...)
	if over := len(o.buf) - jobTail; over > 0 {
		o.buf = append(o.buf[:0:0], o.buf[over:]...)
		o.cut += over
	}
	o.mu.Unlock()
	return len(p), nil
}

func (o *callOut) text() string {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.cut == 0 {
		return string(o.buf)
	}
	return fmt.Sprintf("… [%d earlier bytes] …\n%s", o.cut, o.buf)
}

// watch opens the live tail for a running native call; the returned
// func forgets it. An adopted job keeps its own reference, so its
// output outlives the call.
func (j *Jobs) watch(call string) (*callOut, func()) {
	o := &callOut{}
	if call == "" {
		return o, func() {}
	}
	j.mu.Lock()
	if j.calls == nil {
		j.calls = map[string]*callOut{}
	}
	j.calls[call] = o
	j.mu.Unlock()
	return o, func() {
		j.mu.Lock()
		if j.calls[call] == o {
			delete(j.calls, call)
		}
		j.mu.Unlock()
	}
}

// Adopt numbers work the engine already runs (a call that outlived its
// turn's settle window) as a job, so the strip, /jobkill N and serve's
// job signals see it. It records job{id, event:"started", cmd, call}.
// No notice is ever queued for it: its result reaches the model through
// the engine. finish records job{id, event:"finished", exit?, stopped?,
// call}; only its first call counts.
func (j *Jobs) Adopt(cmd, call string, kill func()) (id int, finish func(exit *int, stopped bool)) {
	owner := j.session()
	j.mu.Lock()
	j.next++
	b := &job{id: j.next, owner: owner, cmd: cmd, started: time.Now(), call: call, kill: kill, live: j.calls[call]}
	j.list = append(j.list, b)
	j.mu.Unlock()
	j.recordJob(b, "started", 0)
	var once sync.Once
	return b.id, func(exit *int, stopped bool) {
		once.Do(func() {
			b.mu.Lock()
			b.done, b.ended, b.exit = true, time.Now(), -1
			if exit != nil {
				b.exit = *exit
				if *exit != 0 {
					b.err = fmt.Sprintf("exit status %d", *exit)
				}
			}
			if stopped {
				b.killed = true
			}
			b.mu.Unlock()
			if j.record == nil {
				return
			}
			data := map[string]any{"id": b.id, "event": "finished", "cmd": b.cmd, "call": call}
			if exit != nil {
				data["exit"] = *exit
			}
			if stopped {
				data["stopped"] = true
			}
			j.record("job", data)
		})
	}
}
