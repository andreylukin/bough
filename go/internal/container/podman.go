package container

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
)

// Podman drives the podman CLI (4.9 on Ubuntu 24.04), rootless. Rootless
// is the point on Linux: the guest's root maps to the host user, so a
// file the orb writes into a bind-mounted worktree is owned by the
// person, not by root, and the host-side git that checkpoints the
// worktree keeps working.
//
// CLI shapes used (docker-compatible):
//
//	podman build -t TAG -f FILE DIR
//	podman run -d --init --name N -v SRC:DST[:ro] --mount type=volume,source=V,target=DST[,readonly] -w DIR -e K=V --cpus N -m MEM -p 127.0.0.1:H:G IMAGE sleep infinity
//	podman start N | stop N | rm -f N | inspect --format {{.State.Status}} N
//	podman exec [-i] [-w DIR] [-e K=V] N ARGV...
//	podman image inspect TAG   (exit status = exists)
//	podman volume create N     (errors "already exists" on a repeat)
//	podman info                (non-zero when rootless setup is broken)
//
// Commit is the same generated-Dockerfile build as Apple's: podman has a
// commit verb, but a build cannot see bind mounts either, and one
// COPY+RUN layer per step is what makes the layer cache useful.
type Podman struct{ Bin string }

func NewPodman() *Podman {
	bin := "podman"
	if p, err := exec.LookPath("podman"); err == nil {
		bin = p
	}
	return &Podman{Bin: bin}
}

func (p *Podman) Name() string { return "podman" }

func (p *Podman) run(ctx context.Context, args ...string) ([]byte, error) {
	var out, errb bytes.Buffer
	cmd := exec.CommandContext(ctx, p.Bin, args...)
	cmd.Stdout, cmd.Stderr = &out, &errb
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(errb.String())
		if msg == "" {
			msg = strings.TrimSpace(out.String())
		}
		return out.Bytes(), fmt.Errorf("container: podman %s: %w: %s", args[0], err, msg)
	}
	return out.Bytes(), nil
}

func (p *Podman) stream(ctx context.Context, log io.Writer, args ...string) error {
	cmd := exec.CommandContext(ctx, p.Bin, args...)
	cmd.Stdout, cmd.Stderr = log, log
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("container: podman %s: %w", args[0], err)
	}
	return nil
}

// Available runs `podman info`, which fails when the binary is missing
// or rootless setup is (no subuid range, no XDG_RUNTIME_DIR).
func (p *Podman) Available(ctx context.Context) error {
	if _, err := exec.LookPath(p.Bin); err != nil {
		return fmt.Errorf("container: podman: not installed: %w (apt install podman)", err)
	}
	if _, err := p.run(ctx, "info", "--format", "{{.Host.Arch}}"); err != nil {
		return fmt.Errorf("%w (rootless podman needs a subuid range for this user and XDG_RUNTIME_DIR)", err)
	}
	return nil
}

func (p *Podman) Build(ctx context.Context, spec BuildSpec, log io.Writer) error {
	return p.stream(ctx, log, podmanBuildArgs(spec)...)
}

func podmanBuildArgs(spec BuildSpec) []string {
	file := spec.Dockerfile
	if file == "" {
		file = filepath.Join(spec.Dir, "Dockerfile")
	}
	return []string{"build", "-t", spec.Tag, "-f", file, spec.Dir}
}

func (p *Podman) Commit(ctx context.Context, spec CommitSpec, log io.Writer) error {
	dir, err := os.MkdirTemp("", "bough-commit-")
	if err != nil {
		return fmt.Errorf("container: podman: commit: %w", err)
	}
	defer os.RemoveAll(dir)
	if err := writeCommitContext(dir, spec); err != nil {
		return fmt.Errorf("container: podman: commit: %w", err)
	}
	return p.Build(ctx, BuildSpec{Dir: dir, Dockerfile: filepath.Join(dir, "Dockerfile"), Tag: spec.Tag}, log)
}

func (p *Podman) ImageExists(ctx context.Context, tag string) (bool, error) {
	err := exec.CommandContext(ctx, p.Bin, "image", "exists", tag).Run()
	if err == nil {
		return true, nil
	}
	if _, ok := err.(*exec.ExitError); ok && ctx.Err() == nil {
		return false, nil
	}
	return false, fmt.Errorf("container: podman: image exists %s: %w", tag, err)
}

func (p *Podman) Start(ctx context.Context, spec RunSpec) error {
	st, err := p.Inspect(ctx, spec.Name)
	if err != nil {
		return err
	}
	switch st {
	case StateRunning:
		return nil
	case StateStopped:
		_, err = p.run(ctx, "start", spec.Name)
		return err
	}
	_, err = p.run(ctx, podmanRunArgs(spec)...)
	return err
}

// podmanRunArgs is runArgs with the one flag that differs: docker-style
// CLIs spell a CPU count --cpus, and -c there is cpu-shares.
func podmanRunArgs(spec RunSpec) []string {
	args := []string{"run", "-d", "--init", "--name", spec.Name}
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
		args = append(args, "--cpus", strconv.Itoa(spec.CPUs))
	}
	if spec.Memory != "" {
		args = append(args, "-m", spec.Memory)
	}
	for _, pm := range spec.Ports {
		args = append(args, "-p", fmt.Sprintf("127.0.0.1:%d:%d", pm.Host, pm.Guest))
	}
	return append(args, spec.Image, "sleep", "infinity")
}

func (p *Podman) Command(ctx context.Context, name string, opt ExecOptions, argv ...string) *exec.Cmd {
	id := fmt.Sprintf("%d-%d", os.Getpid(), execSeq.Add(1))
	opt.Env = append(append([]string(nil), opt.Env...), execIDVar+"="+id)
	cmd := exec.CommandContext(ctx, p.Bin, execArgs(name, opt, argv)...)
	if len(opt.Secrets) > 0 {
		cmd.Env = append(os.Environ(), opt.Secrets...)
	}
	return cmd
}

// KillFunc mirrors Apple.KillFunc: kill the exec client, then every
// guest process carrying this command's BOUGH_EXEC_ID, and nothing else.
func (p *Podman) KillFunc(name string, cmd *exec.Cmd) func() error {
	return func() error {
		var err error
		if cmd.Process != nil {
			err = cmd.Process.Signal(syscall.SIGKILL)
		}
		if id := execID(cmd); id != "" {
			_ = exec.Command(p.Bin, "exec", name, "sh", "-c",
				`for e in /proc/[0-9]*/environ; do p=${e#/proc/}; p=${p%/environ}; [ "$p" = $$ ] && continue; tr '\0' '\n' < "$e" 2>/dev/null | grep -qx "$1" && kill -9 "$p" 2>/dev/null; done; true`,
				"sh", execIDVar+"="+id).Run()
		}
		return err
	}
}

func (p *Podman) Stop(ctx context.Context, name string) error {
	_, err := p.run(ctx, "stop", name)
	return err
}

func (p *Podman) Remove(ctx context.Context, name string) error {
	st, err := p.Inspect(ctx, name)
	if err != nil {
		return err
	}
	if st == StateMissing {
		return nil
	}
	_, err = p.run(ctx, "rm", "-f", name)
	return err
}

// Inspect reads the container's State.Status; an unknown name exits
// non-zero, which is missing.
func (p *Podman) Inspect(ctx context.Context, name string) (State, error) {
	out, err := exec.CommandContext(ctx, p.Bin, "inspect", "--type", "container", "--format", "{{.State.Status}}", name).Output()
	if err != nil {
		if ctx.Err() != nil {
			return StateMissing, fmt.Errorf("container: podman: inspect %s: %w", name, ctx.Err())
		}
		return StateMissing, nil
	}
	return parsePodmanStatus(out), nil
}

func parsePodmanStatus(out []byte) State {
	switch strings.TrimSpace(string(out)) {
	case "":
		return StateMissing
	case "running":
		return StateRunning
	}
	return StateStopped
}

// Address is the container's IPv4 on its first network; rootless podman
// on pasta/slirp reports none, and the caller falls back to -p.
func (p *Podman) Address(ctx context.Context, name string) (string, error) {
	out, err := p.run(ctx, "inspect", "--type", "container", "--format",
		"{{.NetworkSettings.IPAddress}}{{range .NetworkSettings.Networks}} {{.IPAddress}}{{end}}", name)
	if err != nil {
		return "", err
	}
	for _, f := range strings.Fields(string(out)) {
		if ip, _, _ := strings.Cut(f, "/"); ip != "" && ip != "<no value>" {
			return ip, nil
		}
	}
	return "", nil
}

func (p *Podman) Images(ctx context.Context) ([]string, error) {
	out, err := p.run(ctx, "image", "ls", "--format", "{{.Repository}}:{{.Tag}}")
	if err != nil {
		return nil, err
	}
	return parseLines(out), nil
}

func (p *Podman) Running(ctx context.Context) ([]string, error) {
	out, err := p.run(ctx, "ps", "--format", "{{.Names}}") // ps without -a is the running ones
	if err != nil {
		return nil, err
	}
	return parseLines(out), nil
}

func (p *Podman) ContainerImages(ctx context.Context) ([]string, error) {
	out, err := p.run(ctx, "ps", "-a", "--format", "{{.Image}}")
	if err != nil {
		return nil, err
	}
	return parseLines(out), nil
}

// parseLines keeps the non-empty lines that are not a dangling
// "<none>:<none>" image.
func parseLines(out []byte) []string {
	var tags []string
	for _, l := range strings.Split(string(out), "\n") {
		l = strings.TrimSpace(l)
		if l == "" || strings.HasPrefix(l, "<none>") {
			continue
		}
		tags = append(tags, l)
	}
	return tags
}

func (p *Podman) RemoveImage(ctx context.Context, tag string) error {
	_, err := p.run(ctx, "image", "rm", tag)
	return err
}

func (p *Podman) CreateVolume(ctx context.Context, name string) error {
	_, err := p.run(ctx, "volume", "create", name)
	if err != nil && strings.Contains(strings.ToLower(err.Error()), "exist") {
		return nil
	}
	return err
}
