package attention

// The hub: chats as URLs. /s/<id> attaches the browser to a session,
// /s/new starts one, /sessions lists them. The main web session is the
// process this runs in; any other session gets its own `bough --web`
// beside it on a free port, started on first visit and reused after.

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// child is a session served by a process the hub started.
type child struct {
	Session string    `json:"session"`
	Port    int       `json:"port"`
	PID     int       `json:"pid"`
	Since   time.Time `json:"since"`
	cmd     *exec.Cmd
}

// hub is the session router.
type hub struct {
	mu       sync.Mutex
	children map[string]*child // by session id
	mainURL  string            // the main web session (sip), "" when this process is not it
	mainID   func() string     // the main session's id, live (it changes on /new)
	histDir  string
	bin      string
	next     int
}

func newHub(mainURL string, mainID func() string, histDir string) *hub {
	bin, _ := os.Executable()
	return &hub{children: map[string]*child{}, mainURL: mainURL, mainID: mainID, histDir: histDir, bin: bin, next: 7700}
}

// sessionRow is one chat for the page.
type sessionRow struct {
	ID    string    `json:"id"`
	Title string    `json:"title"`
	Cwd   string    `json:"cwd,omitempty"`
	At    time.Time `json:"at"`
	Live  string    `json:"live,omitempty"` // "main", "child", or ""
	URL   string    `json:"url"`
}

// sessions is the list, newest first, with live markers.
func (h *hub) sessions(limit int) []sessionRow {
	infos, err := history.List(h.histDir)
	if err != nil {
		return nil
	}
	h.mu.Lock()
	h.reap()
	main := ""
	if h.mainID != nil {
		main = h.mainID()
	}
	live := map[string]string{}
	for id := range h.children {
		live[id] = "child"
	}
	if main != "" {
		live[main] = "main"
	}
	h.mu.Unlock()
	var out []sessionRow
	for _, in := range infos {
		if in.Entries == 0 && live[in.ID] == "" {
			continue
		}
		out = append(out, sessionRow{ID: in.ID, Title: first(in.Title, "(untitled)"), Cwd: in.Cwd, At: in.ModTime, Live: live[in.ID], URL: "/s/" + in.ID})
		if limit > 0 && len(out) >= limit {
			break
		}
	}
	// The main session first even if quiet.
	sort.SliceStable(out, func(i, j int) bool { return out[i].Live == "main" && out[j].Live != "main" })
	return out
}

// reap forgets children that exited. Caller holds mu.
func (h *hub) reap() {
	for id, c := range h.children {
		if c.cmd != nil && c.cmd.ProcessState != nil {
			delete(h.children, id)
		}
	}
}

// attach returns the URL serving session id, starting a process when
// none does. resume "" with fresh=true starts a new session.
func (h *hub) attach(ctx context.Context, id string, fresh bool, cwd, draft string) (string, error) {
	h.mu.Lock()
	h.reap()
	if !fresh && h.mainID != nil && h.mainID() == id && h.mainURL != "" {
		h.mu.Unlock()
		return h.mainURL, nil
	}
	if c, ok := h.children[id]; ok && !fresh {
		h.mu.Unlock()
		return fmt.Sprintf("http://localhost:%d/", c.Port), nil
	}
	port, err := h.freePort()
	if err != nil {
		h.mu.Unlock()
		return "", err
	}
	h.mu.Unlock()
	if cwd == "" {
		cwd, _ = os.UserHomeDir()
	}
	args := []string{"--web", fmt.Sprintf("localhost:%d", port)}
	if !fresh {
		args = append(args, "--resume", id)
	}
	if draft != "" {
		args = append(args, "--set", "ui.draft="+draft)
	}
	cmd := exec.Command(h.bin, args...)
	cmd.Dir = cwd
	cmd.Env = append(os.Environ(), "BOUGH_WEB_CHILD=1")
	logPath := filepath.Join(filepath.Dir(h.histDir), "web-"+fmt.Sprint(port)+".log")
	if f, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644); err == nil {
		cmd.Stdout, cmd.Stderr = f, f
	}
	if err := cmd.Start(); err != nil {
		return "", fmt.Errorf("start session: %w", err)
	}
	go func() { _ = cmd.Wait() }()
	url := fmt.Sprintf("http://localhost:%d/", port)
	// Ready when it answers; a fresh session's id is learned from the
	// history dir once it writes its file.
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		if r, err := http.Get(url + "health"); err == nil {
			r.Body.Close()
			break
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(150 * time.Millisecond):
		}
	}
	key := id
	if fresh {
		key = "new-" + fmt.Sprint(port)
	}
	h.mu.Lock()
	h.children[key] = &child{Session: key, Port: port, PID: cmd.Process.Pid, Since: time.Now(), cmd: cmd}
	h.mu.Unlock()
	if fresh {
		go h.learnID(key, cmd.Process.Pid)
	}
	return url, nil
}

// learnID re-keys a fresh child by the session file it creates: the
// newest file in the history dir younger than the child.
func (h *hub) learnID(key string, pid int) {
	for i := 0; i < 40; i++ {
		time.Sleep(500 * time.Millisecond)
		infos, err := history.List(h.histDir)
		if err != nil || len(infos) == 0 {
			continue
		}
		h.mu.Lock()
		c, ok := h.children[key]
		if !ok {
			h.mu.Unlock()
			return
		}
		for _, in := range infos {
			if in.ModTime.After(c.Since.Add(-time.Second)) {
				taken := false
				for k, o := range h.children {
					if k == in.ID || (o != c && o.Session == in.ID) {
						taken = true
					}
				}
				if h.mainID != nil && h.mainID() == in.ID {
					taken = true
				}
				if !taken {
					delete(h.children, key)
					c.Session = in.ID
					h.children[in.ID] = c
					h.mu.Unlock()
					return
				}
			}
		}
		h.mu.Unlock()
	}
}

// stop ends every child: the hub's process is going, and a session
// nobody can reach by URL is a session nobody can find.
func (h *hub) stop() {
	h.mu.Lock()
	defer h.mu.Unlock()
	for id, c := range h.children {
		if c.cmd != nil && c.cmd.Process != nil {
			_ = c.cmd.Process.Signal(os.Interrupt)
		}
		delete(h.children, id)
	}
}

// freePort is the next unused port from 7700. Caller holds mu.
func (h *hub) freePort() (int, error) {
	for p := h.next; p < h.next+200; p++ {
		ln, err := net.Listen("tcp", fmt.Sprintf("localhost:%d", p))
		if err != nil {
			continue
		}
		ln.Close()
		h.next = p + 1
		return p, nil
	}
	return 0, fmt.Errorf("no free port in 7700-7900")
}

// routes mounts the hub on mux.
func (h *hub) routes(mux *http.ServeMux) {
	mux.HandleFunc("/sessions", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(h.sessions(40))
	})
	mux.HandleFunc("/s/", func(w http.ResponseWriter, r *http.Request) {
		id := strings.TrimPrefix(r.URL.Path, "/s/")
		q := r.URL.Query()
		ctx, cancel := context.WithTimeout(r.Context(), 20*time.Second)
		defer cancel()
		var url string
		var err error
		if id == "" {
			http.NotFound(w, r)
			return
		}
		if id == "new" {
			url, err = h.attach(ctx, "", true, q.Get("cwd"), q.Get("draft"))
		} else {
			url, err = h.attach(ctx, id, false, "", "")
		}
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadGateway)
			return
		}
		http.Redirect(w, r, url, http.StatusFound)
	})
}
