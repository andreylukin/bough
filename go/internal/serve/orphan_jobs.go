package serve

import (
	"fmt"

	"github.com/andreylukin/bough/plugins/history"
)

// endOrphanJobs records every job the session file still has running as
// stopped. It runs when serve reaps a child: a job's process lives in
// that child and died with it, but nothing in the child said so — a
// SIGKILL (a crash, serve's own restart) runs no unmount, and even a
// clean one never finished an engine call adopted as a job (the actor is
// closed first). Left open, the next child's RunningJobs listed the job
// as running, the adopting turn's footer said "1 call still running"
// forever, and killJob answered ok for a job no process had. Only
// typed entries: the legacy text jobs predate adoption.
func endOrphanJobs(path string) error {
	entries, err := history.Read(path)
	if err != nil {
		return fmt.Errorf("serve: supervisor: end orphaned jobs: %w", err)
	}
	type started struct {
		id   int
		data map[string]any
	}
	var open []started
	for _, e := range entries {
		if !typedJob(e) {
			continue
		}
		id := int(num(e.Data["id"]))
		// Job ids restart with the child: a later start replaces an
		// earlier one of the same id, as in RunningJobs.
		for i, o := range open {
			if o.id == id {
				open = append(open[:i], open[i+1:]...)
				break
			}
		}
		if e.Data["event"] == "started" {
			open = append(open, started{id, e.Data})
		}
	}
	for _, o := range open {
		data := map[string]any{"id": o.id, "event": "finished", "cmd": o.data["cmd"], "stopped": true}
		if call, ok := o.data["call"].(string); ok {
			// The footer counts an adopted call as reported by it.
			data["call"] = call
		}
		if _, err := history.AppendFile(path, "job", data); err != nil {
			return fmt.Errorf("serve: supervisor: end orphaned job %d: %w", o.id, err)
		}
	}
	return nil
}
