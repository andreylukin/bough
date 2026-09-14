package container

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
)

// Fake is an in-memory Runtime for other areas' tests. Command runs argv
// on the HOST in opt.Workdir so exec tests are real.
type Fake struct {
	mu         sync.Mutex
	Calls      []string
	FailBuild  error
	images     map[string]bool
	containers map[string]State
	volumes    map[string]bool
}

func NewFake() *Fake {
	return &Fake{images: map[string]bool{}, containers: map[string]State{}, volumes: map[string]bool{}}
}

func (f *Fake) record(s string) { f.Calls = append(f.Calls, s) }

// CallList returns a copy of Calls under the lock, for concurrent tests.
func (f *Fake) CallList() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.Calls...)
}

func (f *Fake) Name() string                    { return "fake" }
func (f *Fake) Available(context.Context) error { return nil }

func (f *Fake) Build(_ context.Context, spec BuildSpec, log io.Writer) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("build " + spec.Tag)
	if log != nil {
		fmt.Fprintf(log, "fake build %s\n", spec.Tag)
	}
	if f.FailBuild != nil {
		return f.FailBuild
	}
	f.images[spec.Tag] = true
	return nil
}

func (f *Fake) Commit(_ context.Context, spec CommitSpec, log io.Writer) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("commit " + spec.Tag)
	if log != nil {
		fmt.Fprintf(log, "fake commit %s\n", spec.Tag)
	}
	if f.FailBuild != nil {
		return f.FailBuild
	}
	f.images[spec.Tag] = true
	return nil
}

func (f *Fake) ImageExists(_ context.Context, tag string) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("image-exists " + tag)
	return f.images[tag], nil
}

func (f *Fake) Start(_ context.Context, spec RunSpec) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("start " + spec.Name)
	if _, ok := f.containers[spec.Name]; !ok && !f.images[spec.Image] {
		return fmt.Errorf("container: fake: image %s not built", spec.Image)
	}
	f.containers[spec.Name] = StateRunning
	return nil
}

func (f *Fake) Command(ctx context.Context, name string, opt ExecOptions, argv ...string) *exec.Cmd {
	f.mu.Lock()
	f.record("exec " + name + " " + strings.Join(argv, " "))
	running := f.containers[name] == StateRunning
	f.mu.Unlock()
	if len(argv) == 0 {
		argv = []string{"true"}
	}
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = opt.Workdir
	// Host env is kept so PATH resolves; opt.Env overrides like -e does.
	cmd.Env = append(os.Environ(), opt.Env...)
	if !running {
		cmd.Err = fmt.Errorf("container: fake: %s is not running", name)
	}
	return cmd
}

func (f *Fake) Stop(_ context.Context, name string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("stop " + name)
	if _, ok := f.containers[name]; !ok {
		return fmt.Errorf("container: fake: no container %s", name)
	}
	f.containers[name] = StateStopped
	return nil
}

func (f *Fake) Remove(_ context.Context, name string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("remove " + name)
	delete(f.containers, name)
	return nil
}

func (f *Fake) Inspect(_ context.Context, name string) (State, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if s, ok := f.containers[name]; ok {
		return s, nil
	}
	return StateMissing, nil
}

func (f *Fake) CreateVolume(_ context.Context, name string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.record("volume " + name)
	f.volumes[name] = true
	return nil
}
