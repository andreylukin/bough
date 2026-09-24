// Command orbserve is `bough serve --run ADDR` over a container runtime
// the test owns, for the browser walk of specs/orb-image-build.fizz
// (go/tests/web/specs/model/orb-image-build.spec.ts).
//
// A real `bough serve` picks container.Default(), which on darwin is the
// Apple CLI at /opt/homebrew/bin/container whatever PATH says: a browser
// test of the image build would build real images. This is the same API
// handler, guard and embedded page over container.Fake, with serve's
// build goroutine parked where the spec splits (the first image check,
// the Commit), answered over /fake/* as the Go adapter answers its
// holdRuntime (tests/model/mbt/orb_image_build_test.go).
//
// Run as a session child (`--headless --json`, which is how serve spawns
// its Exe) it is a stand-in session that only says it exists: the
// project session "Start anyway" opens is the session-start flow's, and
// a real child would build its orb on the real runtime.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/google/uuid"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/serveclient"
	"github.com/andreylukin/bough/plugins/history"
)

func main() {
	if slices.Contains(os.Args[1:], "--headless") {
		child()
		return
	}
	if len(os.Args) != 4 || os.Args[1] != "serve" || os.Args[2] != "--run" {
		fmt.Fprintln(os.Stderr, "usage: orbserve serve --run ADDR")
		os.Exit(2)
	}
	if err := run(os.Args[3]); err != nil {
		fmt.Fprintln(os.Stderr, "orbserve:", err)
		os.Exit(1)
	}
}

// holdRuntime parks serve's build goroutine: the first ImageExists of
// the armed project's tag without a deadline (page reads carry one, the
// goroutine's context.Background() does not) and every Commit of that
// project's tags. The Commit writes its own line to the build log
// before it parks, so the page can tell a build that got past the image
// check (build.json building, log truncated) from one still syncing.
type holdRuntime struct {
	*container.Fake
	mu     sync.Mutex
	prefix string
	armed  bool
	exists chan error
	commit chan error
}

func (h *holdRuntime) ImageExists(ctx context.Context, tag string) (bool, error) {
	_, deadline := ctx.Deadline()
	h.mu.Lock()
	if deadline || !h.armed || !strings.HasPrefix(tag, h.prefix) {
		h.mu.Unlock()
		return h.Fake.ImageExists(ctx, tag)
	}
	h.armed = false
	ch := make(chan error)
	h.exists = ch
	h.mu.Unlock()
	if err := <-ch; err != nil {
		return false, err
	}
	return h.Fake.ImageExists(ctx, tag)
}

func (h *holdRuntime) Commit(ctx context.Context, spec container.CommitSpec, log io.Writer) error {
	h.mu.Lock()
	if h.prefix == "" || !strings.HasPrefix(spec.Tag, h.prefix) {
		h.mu.Unlock()
		return h.Fake.Commit(ctx, spec, log)
	}
	fmt.Fprintf(log, "fake: building %s\n", spec.Tag)
	ch := make(chan error)
	h.commit = ch
	h.mu.Unlock()
	if err := <-ch; err != nil {
		// A failed build leaves no tag, as the spec assumes.
		return err
	}
	return h.Fake.Commit(ctx, spec, log)
}

func (h *holdRuntime) held() string {
	h.mu.Lock()
	defer h.mu.Unlock()
	switch {
	case h.exists != nil:
		return "syncing"
	case h.commit != nil:
		return "building"
	}
	return "idle"
}

func (h *holdRuntime) release(which string, err error) bool {
	h.mu.Lock()
	var ch chan error
	switch which {
	case "syncing":
		ch, h.exists = h.exists, nil
	case "building":
		ch, h.commit = h.commit, nil
	}
	h.mu.Unlock()
	if ch == nil {
		return false
	}
	ch <- err
	return true
}

func (h *holdRuntime) arm(slug string) {
	h.mu.Lock()
	h.prefix, h.armed = "bough-orb/"+slug+":", true
	h.mu.Unlock()
}

// control is the test's side of the runtime: POST /fake/arm?slug=S
// before a Build that will be accepted, GET /fake/held, POST
// /fake/release?which=syncing|building[&err=why]. Loopback only, like
// everything this binary serves.
func control(rt *holdRuntime) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /fake/arm", func(w http.ResponseWriter, r *http.Request) {
		rt.arm(r.URL.Query().Get("slug"))
		writeJSON(w, map[string]any{"ok": true})
	})
	mux.HandleFunc("GET /fake/held", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, map[string]any{"held": rt.held()})
	})
	mux.HandleFunc("POST /fake/release", func(w http.ResponseWriter, r *http.Request) {
		var err error
		if msg := r.URL.Query().Get("err"); msg != "" {
			err = errors.New(msg)
		}
		writeJSON(w, map[string]any{"released": rt.release(r.URL.Query().Get("which"), err)})
	})
	return mux
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(v)
}

func run(addr string) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	token, err := serveclient.LoadToken(home)
	if err != nil {
		return err
	}
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	rt := &holdRuntime{Fake: container.NewFake()}
	// The base image exists, so a build is the project's Commit alone.
	rt.AddImage(projectdef.BaseTag())
	sup, err := serve.NewSupervisor(serve.Options{
		Exe: exe, Runtime: rt, Home: home,
		HistDir:  filepath.Join(home, ".bough", "history"),
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
	})
	if err != nil {
		return err
	}
	defer sup.Close()
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}
	root := http.NewServeMux()
	root.Handle("/fake/", control(rt))
	root.Handle("/", serve.Guard(serve.NewAPI(sup), token, false, ""))
	srv := &http.Server{Handler: root}
	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigs
		// A parked build would hold its goroutine past shutdown.
		rt.release("syncing", errors.New("serve stopping"))
		rt.release("building", errors.New("serve stopping"))
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		srv.Shutdown(ctx)
	}()
	fmt.Printf("orbserve: http://%s (pid %d)\n", addr, os.Getpid())
	if err := srv.Serve(ln); err != nil && err != http.ErrServerClosed {
		return err
	}
	return nil
}

// child is a session that exists and does nothing: a meta entry in its
// transcript, a meta line on stdout naming it, then input read and
// dropped until serve closes stdin.
func child() {
	// serve names the id of a session it creates with one (a project's
	// main thread) in the environment, and of one it resumes with -r.
	id := os.Getenv("BOUGH_SESSION_ID")
	for i, a := range os.Args {
		if a == "-r" && i+1 < len(os.Args) {
			id = os.Args[i+1]
		}
	}
	if id == "" {
		u, err := uuid.NewV7()
		if err != nil {
			os.Exit(1)
		}
		id = u.String()
	}
	home, _ := os.UserHomeDir()
	path := filepath.Join(home, ".bough", "history", id+".jsonl")
	if _, err := os.Stat(path); err != nil {
		cwd, _ := os.Getwd()
		b, _ := json.Marshal(history.Entry{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{
			"cwd": cwd, "origin": os.Getenv("BOUGH_ORIGIN"), "mode": os.Getenv("BOUGH_MODE"), "project": os.Getenv("BOUGH_PROJECT"),
		}})
		// Whole or not at all: serve returns the id once the file exists,
		// and a page that read it empty got a 404 for the new session.
		os.MkdirAll(filepath.Dir(path), 0o755)
		os.WriteFile(path+".tmp", append(b, '\n'), 0o644)
		os.Rename(path+".tmp", path)
	}
	b, _ := json.Marshal(map[string]any{"kind": "meta", "text": "ready", "session": id})
	fmt.Println(string(b))
	sc := bufio.NewScanner(os.Stdin)
	for sc.Scan() {
	}
}
