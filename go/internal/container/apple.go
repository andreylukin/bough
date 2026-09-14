package container

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"syscall"
)

// Apple drives Apple's `container` CLI (1.1.0, macOS 26).
//
// CLI shapes used (from `--help` on 1.1.0):
//
//	container build -t TAG -f FILE --progress plain DIR
//	container run -d --name N -v SRC:DST[:ro] --mount type=volume,source=V,target=DST[,readonly] -w DIR -e K=V -c CPUS -m MEM IMAGE sleep infinity
//	container start N | stop N | delete --force N | inspect N
//	container exec [-i] [-w DIR] [-e K=V] N ARGV...
//	container image inspect TAG   (exit status = exists)
//	container volume create N
//	container system status       (non-zero when the apiserver is down)
//
// UNVERIFIED live (apiserver was not running when written): inspect's JSON
// field names, `volume create` on an existing name, and exec kill
// semantics; apple_live_test.go asserts them.
//
// There is no commit verb in 1.1.0, so Commit is a generated Dockerfile
// build (see Commit).
type Apple struct{ Bin string }

// NewApple prefers the Homebrew path, because bough children may run with a
// minimal PATH, then falls back to $PATH.
func NewApple() *Apple {
	bin := "/opt/homebrew/bin/container"
	if _, err := os.Stat(bin); err != nil {
		if p, lerr := exec.LookPath("container"); lerr == nil {
			bin = p
		}
	}
	return &Apple{Bin: bin}
}

func (a *Apple) Name() string { return "apple" }

func (a *Apple) run(ctx context.Context, args ...string) ([]byte, error) {
	var out, errb bytes.Buffer
	cmd := exec.CommandContext(ctx, a.Bin, args...)
	cmd.Stdout, cmd.Stderr = &out, &errb
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(errb.String())
		if msg == "" {
			msg = strings.TrimSpace(out.String())
		}
		return out.Bytes(), fmt.Errorf("container: apple: %s: %w: %s", strings.Join(args[:min(2, len(args))], " "), err, msg)
	}
	return out.Bytes(), nil
}

// stream runs the CLI with combined output copied line by line to log, so
// a build log tail is live rather than dumped at the end.
func (a *Apple) stream(ctx context.Context, log io.Writer, args ...string) error {
	cmd := exec.CommandContext(ctx, a.Bin, args...)
	pr, pw := io.Pipe()
	cmd.Stdout, cmd.Stderr = pw, pw
	if log == nil {
		log = io.Discard
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		sc := bufio.NewScanner(pr)
		sc.Buffer(make([]byte, 64*1024), 1024*1024)
		for sc.Scan() {
			fmt.Fprintln(log, sc.Text())
		}
		_, _ = io.Copy(io.Discard, pr)
	}()
	err := cmd.Run()
	pw.Close()
	<-done
	if err != nil {
		return fmt.Errorf("container: apple: %s: %w", args[0], err)
	}
	return nil
}

func (a *Apple) Available(ctx context.Context) error {
	if _, err := os.Stat(a.Bin); err != nil {
		return fmt.Errorf("container: apple: %s not found (run: brew install container): %w", a.Bin, err)
	}
	if _, err := a.run(ctx, "system", "status"); err != nil {
		return fmt.Errorf("%w (run: container system start)", err)
	}
	return nil
}

func (a *Apple) Build(ctx context.Context, spec BuildSpec, log io.Writer) error {
	return a.stream(ctx, log, buildArgs(spec)...)
}

func buildArgs(spec BuildSpec) []string {
	file := spec.Dockerfile
	if file == "" {
		file = filepath.Join(spec.Dir, "Dockerfile")
	}
	return []string{"build", "-t", spec.Tag, "-f", file, "--progress", "plain", spec.Dir}
}

// Commit writes a temp context and a Dockerfile with one COPY+RUN layer
// per step (docs/orb-speed.md §2), then builds it: 1.1.0 has no commit
// verb and a build cannot see bind mounts, so files are copied in instead.
func (a *Apple) Commit(ctx context.Context, spec CommitSpec, log io.Writer) error {
	dir, err := os.MkdirTemp("", "bough-commit-")
	if err != nil {
		return fmt.Errorf("container: apple: commit: %w", err)
	}
	defer os.RemoveAll(dir)
	if err := writeCommitContext(dir, spec); err != nil {
		return fmt.Errorf("container: apple: commit: %w", err)
	}
	return a.Build(ctx, BuildSpec{Dir: dir, Dockerfile: filepath.Join(dir, "Dockerfile"), Tag: spec.Tag}, log)
}

func writeCommitContext(dir string, spec CommitSpec) error {
	if len(spec.Steps) == 0 {
		return fmt.Errorf("no setup steps")
	}
	base := spec.Base
	if base == "" {
		base = DefaultBase
	}
	var df strings.Builder
	fmt.Fprintf(&df, "FROM %s\n", base)
	for i, st := range spec.Steps {
		name := fmt.Sprintf("steps/%02d-%s.sh", i+1, st.Name)
		if err := os.MkdirAll(filepath.Join(dir, "steps"), 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(filepath.Join(dir, name), st.Script, 0o644); err != nil {
			return err
		}
		fmt.Fprintf(&df, "COPY %s /bough-setup/steps/\n", name)
		for _, f := range st.Files {
			r, err := filepath.Rel(spec.FilesRoot, f)
			if err != nil || strings.HasPrefix(r, "..") {
				return fmt.Errorf("file %s is outside %s", f, spec.FilesRoot)
			}
			rel := "lock/" + filepath.ToSlash(r)
			if err := copyFile(f, filepath.Join(dir, rel)); err != nil {
				return err
			}
			fmt.Fprintf(&df, "COPY %s /bough-setup/%s\n", rel, rel)
		}
		fmt.Fprintf(&df, "RUN %s\n", strings.Join(ScriptArgv(st.Script, "/bough-setup/"+name), " "))
	}
	return os.WriteFile(filepath.Join(dir, "Dockerfile"), []byte(df.String()), 0o644)
}

func copyFile(src, dst string) error {
	b, err := os.ReadFile(src)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
		return err
	}
	return os.WriteFile(dst, b, 0o644)
}

func (a *Apple) ImageExists(ctx context.Context, tag string) (bool, error) {
	cmd := exec.CommandContext(ctx, a.Bin, "image", "inspect", tag)
	out, err := cmd.Output()
	if err != nil {
		if _, ok := err.(*exec.ExitError); ok && ctx.Err() == nil {
			return false, nil
		}
		return false, fmt.Errorf("container: apple: image inspect %s: %w", tag, err)
	}
	// Some CLI versions print [] with exit 0 for an unknown tag.
	t := strings.TrimSpace(string(out))
	return t != "" && t != "[]", nil
}

func (a *Apple) Start(ctx context.Context, spec RunSpec) error {
	st, err := a.Inspect(ctx, spec.Name)
	if err != nil {
		return err
	}
	switch st {
	case StateRunning:
		return nil
	case StateStopped:
		_, err = a.run(ctx, "start", spec.Name)
		return err
	}
	_, err = a.run(ctx, runArgs(spec)...)
	return err
}

func runArgs(spec RunSpec) []string {
	args := []string{"run", "-d", "--name", spec.Name}
	for _, m := range spec.Mounts {
		if m.Volume {
			v := "type=volume,source=" + m.Source + ",target=" + m.Target
			if m.ReadOnly {
				v += ",readonly"
			}
			args = append(args, "--mount", v)
			continue
		}
		v := m.Source + ":" + m.Target
		if m.ReadOnly {
			v += ":ro"
		}
		args = append(args, "-v", v)
	}
	if spec.Workdir != "" {
		args = append(args, "-w", spec.Workdir)
	}
	for _, e := range spec.Env {
		args = append(args, "-e", e)
	}
	if spec.CPUs > 0 {
		args = append(args, "-c", strconv.Itoa(spec.CPUs))
	}
	if spec.Memory != "" {
		args = append(args, "-m", spec.Memory)
	}
	return append(args, spec.Image, "sleep", "infinity")
}

// execIDVar tags every guest process tree started by Command, so KillFunc
// can kill just that command's processes (descendants inherit the env)
// instead of every process in the orb, which would take background jobs
// and concurrent subagent commands down with it.
const execIDVar = "BOUGH_EXEC_ID"

var execSeq atomic.Uint64

func (a *Apple) Command(ctx context.Context, name string, opt ExecOptions, argv ...string) *exec.Cmd {
	id := fmt.Sprintf("%d-%d", os.Getpid(), execSeq.Add(1))
	opt.Env = append(append([]string(nil), opt.Env...), execIDVar+"="+id)
	cmd := exec.CommandContext(ctx, a.Bin, execArgs(name, opt, argv)...)
	if len(opt.Secrets) > 0 {
		cmd.Env = append(os.Environ(), opt.Secrets...)
	}
	return cmd
}

// execID recovers the tag Command put in cmd's argv.
func execID(cmd *exec.Cmd) string {
	for i, arg := range cmd.Args {
		if v, ok := strings.CutPrefix(arg, execIDVar+"="); ok && i > 0 && cmd.Args[i-1] == "-e" {
			return v
		}
	}
	return ""
}

func execArgs(name string, opt ExecOptions, argv []string) []string {
	args := []string{"exec"}
	if opt.Interactive {
		args = append(args, "-i")
	}
	if opt.Workdir != "" {
		args = append(args, "-w", opt.Workdir)
	}
	for _, e := range opt.Env {
		args = append(args, "-e", e)
	}
	for _, e := range opt.Secrets {
		k, _, _ := strings.Cut(e, "=")
		args = append(args, "-e", k)
	}
	args = append(args, name)
	return append(args, argv...)
}

// KillFunc is a Cancel func for a Command cmd: it kills the exec client and,
// because killing the client may not end the guest process (unverified on
// 1.1.0), also kills every guest process carrying cmd's BOUGH_EXEC_ID.
// Other commands in the same orb (background jobs) are left alone.
func (a *Apple) KillFunc(name string, cmd *exec.Cmd) func() error {
	return func() error {
		var err error
		if cmd.Process != nil {
			err = cmd.Process.Signal(syscall.SIGKILL)
		}
		if id := execID(cmd); id != "" {
			_ = exec.Command(a.Bin, "exec", name, "sh", "-c",
				`for e in /proc/[0-9]*/environ; do p=${e#/proc/}; p=${p%/environ}; [ "$p" = $$ ] && continue; tr '\0' '\n' < "$e" 2>/dev/null | grep -qx "$1" && kill -9 "$p" 2>/dev/null; done; true`,
				"sh", execIDVar+"="+id).Run()
		}
		return err
	}
}

func (a *Apple) Stop(ctx context.Context, name string) error {
	_, err := a.run(ctx, "stop", name)
	return err
}

func (a *Apple) Remove(ctx context.Context, name string) error {
	st, err := a.Inspect(ctx, name)
	if err != nil {
		return err
	}
	if st == StateMissing {
		return nil
	}
	_, err = a.run(ctx, "delete", "--force", name)
	return err
}

// Inspect parses `container inspect N`'s default JSON. A failing inspect
// or empty array means missing, since 1.1.0 errors on unknown ids.
func (a *Apple) Inspect(ctx context.Context, name string) (State, error) {
	out, err := exec.CommandContext(ctx, a.Bin, "inspect", name).Output()
	if err != nil {
		if ctx.Err() != nil {
			return StateMissing, fmt.Errorf("container: apple: inspect %s: %w", name, ctx.Err())
		}
		return StateMissing, nil
	}
	return parseInspect(out)
}

func parseInspect(out []byte) (State, error) {
	var items []map[string]any
	if len(bytes.TrimSpace(out)) == 0 {
		return StateMissing, nil
	}
	if err := json.Unmarshal(out, &items); err != nil {
		return StateMissing, fmt.Errorf("container: apple: inspect json: %w", err)
	}
	if len(items) == 0 {
		return StateMissing, nil
	}
	s, _ := items[0]["status"].(string)
	// 1.1.0 live prints status as an object: {"state":"running",...}.
	if obj, ok := items[0]["status"].(map[string]any); ok {
		s, _ = obj["state"].(string)
	}
	if strings.EqualFold(s, "running") {
		return StateRunning, nil
	}
	return StateStopped, nil
}

// Images reads `container image list --format json`: the tag is
// configuration.name (checked on 1.1.0 and 1.4.1).
func (a *Apple) Images(ctx context.Context) ([]string, error) {
	out, err := a.run(ctx, "image", "list", "--format", "json")
	if err != nil {
		return nil, err
	}
	return parseImageList(out)
}

func parseImageList(out []byte) ([]string, error) {
	var items []struct {
		Configuration struct {
			Name string `json:"name"`
		} `json:"configuration"`
	}
	if err := json.Unmarshal(out, &items); err != nil {
		return nil, fmt.Errorf("container: apple: image list json: %w", err)
	}
	var tags []string
	for _, it := range items {
		if it.Configuration.Name != "" {
			tags = append(tags, it.Configuration.Name)
		}
	}
	return tags, nil
}

// ContainerImages reads `container list --all --format json`: the image
// is configuration.image.reference (checked on 1.1.0 and 1.4.1).
func (a *Apple) ContainerImages(ctx context.Context) ([]string, error) {
	out, err := a.run(ctx, "list", "--all", "--format", "json")
	if err != nil {
		return nil, err
	}
	return parseContainerImages(out)
}

func parseContainerImages(out []byte) ([]string, error) {
	var items []struct {
		Configuration struct {
			Image struct {
				Reference string `json:"reference"`
			} `json:"image"`
		} `json:"configuration"`
	}
	if err := json.Unmarshal(out, &items); err != nil {
		return nil, fmt.Errorf("container: apple: list json: %w", err)
	}
	var refs []string
	for _, it := range items {
		if r := it.Configuration.Image.Reference; r != "" {
			refs = append(refs, r)
		}
	}
	return refs, nil
}

func (a *Apple) RemoveImage(ctx context.Context, tag string) error {
	_, err := a.run(ctx, "image", "delete", tag)
	return err
}

func (a *Apple) CreateVolume(ctx context.Context, name string) error {
	_, err := a.run(ctx, "volume", "create", name)
	if err != nil && strings.Contains(strings.ToLower(err.Error()), "exist") {
		return nil
	}
	return err
}
