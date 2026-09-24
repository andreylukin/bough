// Package testbin hands test suites the bough binary they exec: one
// build shared by every package of a `go test` run, and by the next
// run too while the sources are unchanged.
//
// e2e, vtreal and servetest each used to `go build` into a fresh temp
// dir in their own process, so `go test ./...` linked the same ~100 MB
// binary three times and every rerun paid for it again. The binary is
// now cached under os.TempDir by a hash of exactly what goes into it,
// so concurrent packages and later runs reuse one file. BOUGH_BIN still
// overrides (CI builds once in an earlier step).
package testbin

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"
)

// Path is the bough binary to exec: BOUGH_BIN when set, else the
// cached build of this checkout's ./cmd/bough. Resolved once per
// process.
func Path() (string, error) { return path() }

var path = sync.OnceValues(func() (string, error) {
	return resolve(os.Getenv("BOUGH_BIN"), func() (string, error) {
		_, file, _, ok := runtime.Caller(0)
		if !ok {
			return "", errors.New("testbin: cannot locate source file")
		}
		module := filepath.Join(filepath.Dir(file), "..", "..")
		return Build(module, "./cmd/bough", filepath.Join(os.TempDir(), "bough-testbin"))
	})
})

func resolve(env string, build func() (string, error)) (string, error) {
	if env != "" {
		return env, nil
	}
	return build()
}

// Key hashes what the binary of pkg is built from: the non-test Go and
// assembly files and the //go:embed files of every package of this
// module it links, go.mod, go.sum, and the toolchain and target. Only
// those — hashing the tree would rebuild after every edit to a test,
// a doc or tests/web output, and hashing Go files alone would reuse a
// binary whose embedded web/dist is stale.
func Key(module, pkg string) (string, error) {
	list := exec.Command("go", "list", "-deps",
		"-f", "{{if .Module}}{{if .Module.Main}}{{.Dir}}\t{{join .GoFiles \" \"}} {{join .SFiles \" \"}} {{join .EmbedFiles \" \"}}{{end}}{{end}}",
		pkg)
	list.Dir = module
	var stderr bytes.Buffer
	list.Stderr = &stderr
	out, err := list.Output()
	if err != nil {
		return "", fmt.Errorf("testbin: go list: %v\n%s", err, stderr.Bytes())
	}
	files := []string{filepath.Join(module, "go.mod"), filepath.Join(module, "go.sum")}
	sc := bufio.NewScanner(bytes.NewReader(out))
	for sc.Scan() {
		dir, names, _ := strings.Cut(sc.Text(), "\t")
		for _, n := range strings.Fields(names) {
			files = append(files, filepath.Join(dir, n))
		}
	}
	sort.Strings(files)

	h := sha256.New()
	// The toolchain and the env that changes what `go build` emits.
	fmt.Fprintf(h, "%s %s/%s\n", runtime.Version(), runtime.GOOS, runtime.GOARCH)
	for _, k := range []string{"GOFLAGS", "CGO_ENABLED", "GOEXPERIMENT", "GOAMD64", "GOARM64", "GOTOOLCHAIN"} {
		fmt.Fprintf(h, "%s=%s\n", k, os.Getenv(k))
	}
	for _, f := range files {
		rel, err := filepath.Rel(module, f)
		if err != nil {
			rel = f
		}
		fmt.Fprintf(h, "%s\x00", filepath.ToSlash(rel))
		fh, err := os.Open(f)
		if errors.Is(err, os.ErrNotExist) && strings.HasSuffix(f, "go.sum") {
			continue // a module with no dependencies has none
		}
		if err != nil {
			return "", fmt.Errorf("testbin: %v", err)
		}
		_, err = io.Copy(h, fh)
		fh.Close()
		if err != nil {
			return "", fmt.Errorf("testbin: %v", err)
		}
		h.Write([]byte{0})
	}
	return hex.EncodeToString(h.Sum(nil))[:20], nil
}

// Build returns root/<key>/<name>, building it only when absent.
func Build(module, pkg, root string) (string, error) {
	return build(module, pkg, root, "go")
}

func build(module, pkg, root, gocmd string) (string, error) {
	key, err := Key(module, pkg)
	if err != nil {
		return "", err
	}
	dir := filepath.Join(root, key)
	bin := filepath.Join(dir, "bough"+exeSuffix())
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", fmt.Errorf("testbin: %v", err)
	}
	// One builder per key: packages of one `go test ./...` start
	// together, and without the lock each would link its own copy.
	unlock, err := lock(filepath.Join(dir, "lock"))
	if err != nil {
		return "", fmt.Errorf("testbin: lock: %v", err)
	}
	defer unlock()
	now := time.Now()
	if fi, err := os.Stat(bin); err == nil && fi.Mode().IsRegular() {
		_ = os.Chtimes(dir, now, now) // keeps it out of prune's reach
		return bin, nil
	}
	// Link to a temp name and rename into place: a half-written file
	// must never be found by the Stat above, and writing over a binary
	// another process is running gets it SIGKILLed on macOS.
	tmp := filepath.Join(dir, fmt.Sprintf("bough-%d.tmp%s", os.Getpid(), exeSuffix()))
	// -buildvcs=false: the key ignores commits, so a stamped revision
	// would go stale on reuse; unstamped says "dev" honestly.
	cmd := exec.Command(gocmd, "build", "-buildvcs=false", "-o", tmp, pkg)
	cmd.Dir = module
	if out, err := cmd.CombinedOutput(); err != nil {
		os.Remove(tmp)
		return "", fmt.Errorf("testbin: go build %s: %v\n%s", pkg, err, out)
	}
	if err := os.Rename(tmp, bin); err != nil {
		os.Remove(tmp)
		return "", fmt.Errorf("testbin: %v", err)
	}
	prune(root, key, now)
	return bin, nil
}

// prune drops builds of other keys not used for a day: each source
// edit makes a new ~100 MB entry, and nothing else would remove them.
func prune(root, keep string, now time.Time) {
	entries, err := os.ReadDir(root)
	if err != nil {
		return
	}
	for _, e := range entries {
		if !e.IsDir() || e.Name() == keep {
			continue
		}
		if fi, err := e.Info(); err == nil && now.Sub(fi.ModTime()) > 24*time.Hour {
			_ = os.RemoveAll(filepath.Join(root, e.Name()))
		}
	}
}

func exeSuffix() string {
	if runtime.GOOS == "windows" {
		return ".exe"
	}
	return ""
}
