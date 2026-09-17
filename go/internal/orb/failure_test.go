package orb

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

func TestErrorLines(t *testing.T) {
	t.Parallel()
	log := "#1 [internal] load build definition\n#5 0.1 Reading package lists...\n\n" +
		"#5 1.2 E: Unable to locate package definitely-not-a-pkg\n" +
		"#5 ERROR: process \"/bin/sh -c apt-get install definitely-not-a-pkg\" did not complete successfully: exit code: 100\n" +
		"\x1b[31mError: failed to solve\x1b[0m\r\n\nbuild failed: exit status 1\n"
	got := ErrorLines(log, 4)
	want := []string{
		"#5 1.2 E: Unable to locate package definitely-not-a-pkg",
		"#5 ERROR: process \"/bin/sh -c apt-get install definitely-not-a-pkg\" did not complete successfully: exit code: 100",
		"Error: failed to solve",
	}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("got %q", got)
	}
	// No line looks like an error: the last lines are the best guess.
	if got := ErrorLines("== resume.sh start x\na\nb\nc\n== resume.sh end y: exit status 1\n", 2); strings.Join(got, "|") != "b|c" {
		t.Fatalf("plain tail %q", got)
	}
	if got := ErrorLines("", 3); len(got) != 0 {
		t.Fatalf("empty %q", got)
	}
}

// A failed build names what failed, not just "exit status 1", and says
// where the whole log is.
func TestEnsureImageFailureCarriesLogLines(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "bad2", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	if err := rt.Build(context.Background(), container.BuildSpec{Tag: projectdef.BaseTag()}, nil); err != nil {
		t.Fatal(err)
	}
	rt.FailBuild = errors.New("exit status 1")
	rt.FailBuildLog = "Reading package lists...\nE: Unable to locate package definitely-not-a-pkg\n"
	_, err := EnsureImage(context.Background(), rt, home, p, nil)
	if err == nil || !strings.Contains(err.Error(), "E: Unable to locate package definitely-not-a-pkg") || !strings.Contains(err.Error(), ImageLogPath(home, "bad2")) {
		t.Fatalf("err %v", err)
	}
	if b, _ := ReadBuild(home, "bad2"); !strings.Contains(b.Error, "Unable to locate package") {
		t.Fatalf("build.json error %q", b.Error)
	}
	st, _ := func() (State, error) {
		_, oerr := Open(context.Background(), rt, home, "s9", p, "")
		if oerr == nil {
			t.Fatal("open ok")
		}
		return ReadState(home, "s9")
	}()
	if st.Phase != PhaseBuild || !strings.Contains(st.Error, "Unable to locate package") {
		t.Fatalf("state %+v", st)
	}
}

func TestOpenResumeFailureCarriesLogLines(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "rf2", "  - path: "+newRepo(t)+"\n")
	projectdef.WriteFile(home, "rf2", projectdef.FileResume, "#!/bin/sh\necho installing\necho 'npm ERR! missing script: dev' >&2\nexit 3\n")
	o, err := Open(context.Background(), container.NewFake(), home, "s8", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	st := o.State()
	if st.Phase != PhaseSetup || !strings.Contains(st.Error, "npm ERR! missing script: dev") || strings.Contains(st.Error, "== resume.sh") {
		t.Fatalf("state %+v", st)
	}
}

func TestOpenStartFailurePhase(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "sf", "  - path: "+newRepo(t)+"\n")
	p.Def.Repos[0].Path = home + "/gone"
	if _, err := Open(context.Background(), container.NewFake(), home, "s7", p, ""); err == nil {
		t.Fatal("open ok")
	}
	if st, _ := ReadState(home, "s7"); st.Phase != PhaseStart {
		t.Fatalf("state %+v", st)
	}
}
