package main

import "testing"

func TestResolveMode(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name          string
		in            modeInputs
		mode, project string
		notice, err   bool
	}{
		{name: "default local", in: modeInputs{}, mode: "local"},
		{name: "flag project", in: modeInputs{FlagProject: "web"}, mode: "project", project: "web"},
		{name: "flag local", in: modeInputs{FlagLocal: true, EnvMode: "project", EnvProject: "web"}, mode: "local"},
		{name: "both flags", in: modeInputs{FlagProject: "web", FlagLocal: true}, err: true},
		{name: "env project", in: modeInputs{EnvMode: "project", EnvProject: "api"}, mode: "project", project: "api"},
		{name: "flag beats env", in: modeInputs{FlagProject: "web", EnvMode: "project", EnvProject: "api"}, mode: "project", project: "web"},
		{name: "env project without slug", in: modeInputs{EnvMode: "project"}, err: true},
		{name: "env bad mode", in: modeInputs{EnvMode: "cloud"}, err: true},
		{name: "env local", in: modeInputs{EnvMode: "local"}, mode: "local"},
		{name: "resume old meta", in: modeInputs{Resumed: true}, mode: "local"},
		{name: "resume project meta beats env", in: modeInputs{Resumed: true, MetaMode: "project", MetaProject: "web", EnvMode: "local"}, mode: "project", project: "web"},
		{name: "resume disagreeing flag", in: modeInputs{Resumed: true, MetaMode: "local", FlagProject: "web"}, mode: "local", notice: true},
		{name: "resume agreeing flag", in: modeInputs{Resumed: true, MetaMode: "project", MetaProject: "web", FlagProject: "web"}, mode: "project", project: "web"},
	} {
		mode, project, notice, err := resolveMode(tc.in)
		if (err != nil) != tc.err {
			t.Errorf("%s: err = %v", tc.name, err)
			continue
		}
		if mode != tc.mode || project != tc.project || (notice != "") != tc.notice {
			t.Errorf("%s: got %q %q notice=%q", tc.name, mode, project, notice)
		}
	}
}

func TestSessionFileTakesLastOverride(t *testing.T) {
	t.Parallel()
	if got := sessionFile(setFlags{"history.file=/a", "llm.model=x", "history.file=/b"}); got != "/b" {
		t.Errorf("sessionFile = %q", got)
	}
}
