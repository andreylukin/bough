package orb

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

// A restart applies a project's current definition to a session that is
// already running, without a new conversation: Successor builds what the
// definition asks for while the old container keeps serving the session,
// and Replace swaps the containers once the session can spare its shell.
// See docs/orbs.md §1c.

// specKey hashes what a container fixes at create: image, mounts (caches
// and identity dirs included), base env without the token, workdir, cpus,
// memory and the ports project.yml ASKS for. The asked-for list, not
// spec.Ports: a busy host port would otherwise flip the key and recreate
// the container on every start.
func specKey(s container.RunSpec, ports []projectdef.Port) string {
	b, _ := json.Marshal(keyed(s, ports))
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])[:16]
}

type keyedSpec struct {
	Image   string
	Mounts  []container.Mount
	Env     []string
	Workdir string
	CPUs    int
	Memory  string
	Ports   []projectdef.Port
}

// keyed is the part of a spec specKey hashes, with nil and empty lists
// made one so a definition read twice keys the same.
func keyed(s container.RunSpec, ports []projectdef.Port) keyedSpec {
	env := slices.DeleteFunc(slices.Clone(s.Env), func(e string) bool { return strings.HasPrefix(e, tokenEnv+"=") })
	k := keyedSpec{Image: s.Image, Workdir: s.Workdir, CPUs: s.CPUs, Memory: s.Memory}
	if len(s.Mounts) > 0 {
		k.Mounts = s.Mounts
	}
	if len(env) > 0 {
		k.Env = env
	}
	if len(ports) > 0 {
		k.Ports = ports
	}
	return k
}

// specDiff names what differs between two specs, for the restart notice.
func specDiff(a, b container.RunSpec, pa, pb []projectdef.Port) []string {
	ka, kb := keyed(a, pa), keyed(b, pb)
	var out []string
	if ka.Image != kb.Image {
		out = append(out, "image")
	}
	if !slices.Equal(ka.Mounts, kb.Mounts) {
		out = append(out, "mounts")
	}
	if !slices.Equal(ka.Env, kb.Env) {
		out = append(out, "env")
	}
	if ka.Workdir != kb.Workdir {
		out = append(out, "workdir")
	}
	if ka.CPUs != kb.CPUs {
		out = append(out, "cpus")
	}
	if ka.Memory != kb.Memory {
		out = append(out, "memory")
	}
	if !slices.Equal(ka.Ports, kb.Ports) {
		out = append(out, "ports")
	}
	return out
}

// Successor prepares the orb that replaces o under p: repos synced,
// worktrees for new repos added, image built — with o's container still
// running. It writes no state.json and touches no container, so a
// failure here leaves the session exactly as it was. A changed first
// repo is refused: the process's cwd, its checkpoints and the
// container's workdir all hang off it.
func (o *Orb) Successor(ctx context.Context, p projectdef.Project) (*Orb, error) {
	cur := o.State()
	wrap := func(err error) error { return fmt.Errorf("orb: restart %s: %w", o.session, err) }
	primary := Dir(o.home, o.session)
	if len(p.Def.Repos) > 0 {
		primary = filepath.Join(primary, p.Def.Repos[0].RepoName())
	}
	if cur.Primary != "" && primary != cur.Primary {
		return nil, wrap(fmt.Errorf("the first repo changed (%s → %s); start a new session for it", filepath.Base(cur.Primary), filepath.Base(primary)))
	}
	n := &Orb{rt: o.rt, home: o.home, session: o.session, project: p, scratch: o.scratch}
	n.state = State{Session: o.session, Project: p.Slug, Container: cur.Container, PID: os.Getpid()}
	if n.state.Container == "" {
		n.state.Container = container.OrbName(o.session)
	}
	if err := SyncRepos(ctx, o.home, p); err != nil {
		return nil, wrap(err)
	}
	mounts, err := n.prepareMounts(ctx)
	if err != nil {
		return nil, wrap(err)
	}
	n.mounts = mounts
	if err := n.build(ctx); err != nil {
		return nil, wrap(err)
	}
	return n, nil
}

// Replaced is what a Replace did, for the notice the session gets.
type Replaced struct {
	PrevImage, Image string
	Recreated        bool
	Why              []string // what differed (specDiff), or "fresh"
	State            State    // the new orb's state after resume.sh
	// Stopped is whether the old container was stopped, set on the
	// error path too: a Replace that failed at the stop left every job
	// running, one that failed later (and rolled back) killed them.
	Stopped bool
}

// rollbackTimeout bounds bringing the old container back. It is its own
// budget, never the caller's: the usual reason to roll back is that the
// caller's deadline ran out mid-create, and a rollback on that same
// context would fail at once and leave the half-made container behind.
const rollbackTimeout = 2 * time.Minute

// Replace swaps o's container for next's (from o.Successor): stop o
// (state.json says stopped first, as Stop does, so its jobs know why they
// died), remove the container only when the image, the spec or fresh says
// so, then start next, and resume.sh reruns. If next fails to start, o's
// container is brought back from o.spec and the error says both.
// Worktrees, scratch and caches are never touched. o is retired in the
// same critical section that stops it, so an exec waiting on o.mu gets
// ErrReplaced instead of starting the old container again under the
// new one; it stays retired unless the rollback brings it back.
func (o *Orb) Replace(ctx context.Context, next *Orb, fresh bool) (Replaced, error) {
	o.mu.Lock()
	prev := o.state
	oldSpec := o.spec
	o.mu.Unlock()
	name := oldSpec.Name
	if name == "" {
		name = prev.Container
	}
	newKey := specKey(next.spec, next.project.Def.Ports)
	res := Replaced{PrevImage: prev.Image, Why: specDiff(oldSpec, next.spec, o.project.Def.Ports, next.project.Def.Ports)}
	res.Recreated = fresh || prev.Image != next.spec.Image || prev.Spec == "" || prev.Spec != newKey
	if fresh {
		res.Why = append(res.Why, "fresh")
	}
	// Portals are recorded straight into state.json (recordPortal), not
	// into o.state: carry them over, and RetargetPortals keeps their URLs.
	if disk, err := ReadState(o.home, o.session); err == nil {
		next.state.Portals = disk.Portals
	}
	o.mu.Lock()
	if err := o.stopLocked(ctx); err != nil {
		o.mu.Unlock()
		return Replaced{}, fmt.Errorf("orb: restart %s: %w", o.session, err)
	}
	o.retired = true
	o.mu.Unlock()
	back := func(cause error) (Replaced, error) {
		rctx, cancel := context.WithTimeout(context.Background(), rollbackTimeout)
		defer cancel()
		o.mu.Lock()
		defer o.mu.Unlock()
		if res.Recreated {
			// A half-made new container must not be restarted as ours:
			// if it cannot be removed, o stays retired and fails its
			// execs rather than adopt it.
			if err := o.rt.Remove(rctx, name); err != nil {
				return Replaced{Stopped: true}, fmt.Errorf("orb: restart %s: %w; the old container did not come back either: remove the new one: %v", o.session, cause, err)
			}
		}
		o.retired = false
		if err := o.ensureRunningLocked(rctx); err != nil {
			return Replaced{Stopped: true}, fmt.Errorf("orb: restart %s: %w; the old container did not come back either: %v", o.session, cause, err)
		}
		return Replaced{Stopped: true}, fmt.Errorf("orb: restart %s: %w (the old container runs again)", o.session, cause)
	}
	if res.Recreated {
		if err := o.rt.Remove(ctx, name); err != nil {
			return back(fmt.Errorf("remove %s: %w", name, err))
		}
	}
	next.prev, next.prevErr = prev, nil
	if err := next.run(ctx); err != nil {
		return back(fmt.Errorf("start the new container: %w", errors.Unwrap(err)))
	}
	st := next.State()
	RetargetPortals(o.session, st.IP)
	res.Image, res.State, res.Stopped = st.Image, st, true
	return res, nil
}

// SetRestart records a requested restart's phase in state.json
// (RestartPending, RestartBuilding, or "" when it is over), where the
// bar, serve and `bough project status` read it. Only that field is
// written: a build takes minutes, and in that time serve's stop or the
// quiet-orb reaper may have marked the container stopped on disk, which
// writing this orb's whole in-memory state back would undo.
func (o *Orb) SetRestart(phase string) {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.retired {
		return
	}
	o.state.Restart = phase
	disk, err := ReadState(o.home, o.session)
	if err != nil || disk.Session != o.session {
		writeState(o.home, o.state)
		return
	}
	disk.Restart = phase
	writeState(o.home, disk)
}

// RestartRequest is ~/.bough/orbs/<session>/restart.json: a restart asked
// for from outside the owning session process (the CLI, the guest's
// relayed `bough project restart`, serve). The owner polls for it.
type RestartRequest struct {
	Fresh bool      `json:"fresh,omitempty"`
	By    string    `json:"by"` // "agent", "cli", "web" or "person"
	At    time.Time `json:"at"`
}

const restartFile = "restart.json"

// RequestRestart writes the session's restart.json, merged with one
// already waiting: Fresh is ORed, the rest is the newer request's.
func RequestRestart(home, session string, r RestartRequest) error {
	if session == "" || strings.ContainsAny(session, `/\`) {
		return fmt.Errorf("orb: restart: bad session id %q", session)
	}
	if r.At.IsZero() {
		r.At = time.Now().UTC()
	}
	path := filepath.Join(Dir(home, session), restartFile)
	var prev RestartRequest
	if err := readJSON(path, &prev); err == nil {
		r.Fresh = r.Fresh || prev.Fresh
	}
	if err := writeJSON(path, r); err != nil {
		return fmt.Errorf("orb: restart %s: %w", session, err)
	}
	return nil
}

// TakeRestart reads and removes the session's restart.json; ok is false
// when there is none. The file is renamed away before it is read, so a
// request written meanwhile lands in a new file instead of being lost.
func TakeRestart(home, session string) (RestartRequest, bool) {
	path := filepath.Join(Dir(home, session), restartFile)
	taken := path + ".taken"
	if err := os.Rename(path, taken); err != nil {
		return RestartRequest{}, false
	}
	defer os.Remove(taken)
	var r RestartRequest
	if b, err := os.ReadFile(taken); err != nil || json.Unmarshal(b, &r) != nil {
		return RestartRequest{}, false
	}
	return r, true
}

// ResumeTail is the last run's resume.log: the lines from the last
// "== resume.sh start" marker on, at most n of them (the last n).
func ResumeTail(home, session string, n int) string {
	b, err := os.ReadFile(filepath.Join(Dir(home, session), "resume.log"))
	if err != nil {
		return ""
	}
	text := string(b)
	if i := strings.LastIndex(text, "== resume.sh start"); i >= 0 {
		text = text[i:]
	}
	lines := strings.Split(strings.TrimRight(text, "\n"), "\n")
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return strings.Join(lines, "\n")
}
