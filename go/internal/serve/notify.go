package serve

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/andreylukin/bough/plugins/history"
)

// Notify tells session id something without prompting it: a live child
// gets a {"notice"} stdin line (queued, wakes it if idle); a session
// with no process gets a "notice" history entry its loop delivers on
// next mount. It never starts a process.
func (s *Supervisor) Notify(id, text string) error { return s.notifyFrom(id, "", text) }

// notifyFrom is Notify with the sending child's id recorded on a stored
// notice.
func (s *Supervisor) notifyFrom(id, from, text string) error {
	s.mu.Lock()
	ch, live := s.kids[id]
	s.mu.Unlock()
	// Not Send: that refuses on a pending ask and would ensure a child.
	// A lease reserved but still starting gets the line once its stdin
	// exists: a stored notice would land after the new process's loop
	// has already read its file, and wait for a later mount.
	if live && ch.started() && s.writeLine(ch, map[string]string{"notice": text}) == nil {
		s.mu.Lock()
		s.emitLocked(id, "notice", text, nil)
		s.mu.Unlock()
		return nil
	}
	path := filepath.Join(s.opt.HistDir, id+".jsonl")
	if _, err := os.Stat(path); errors.Is(err, fs.ErrNotExist) {
		return ErrUnknownSession
	}
	_, err := history.AppendFile(path, "notice", map[string]any{"id": history.NewID(), "to": id, "text": text, "from": from})
	if err != nil {
		return fmt.Errorf("serve: supervisor: notify %s: %w", id, err)
	}
	return nil
}

// writeLine writes v as one raw JSON line, bypassing write's prompt
// wrapping.
func (s *Supervisor) writeLine(ch *child, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("serve: supervisor: encode line: %w", err)
	}
	ch.inMu.Lock()
	defer ch.inMu.Unlock()
	if ch.stdin == nil {
		return fmt.Errorf("serve: supervisor: session has no stdin")
	}
	if _, err := io.WriteString(ch.stdin, string(b)+"\n"); err != nil {
		return fmt.Errorf("serve: supervisor: write stdin: %w", err)
	}
	return nil
}
