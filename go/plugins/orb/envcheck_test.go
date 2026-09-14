package orb

import (
	"reflect"
	"strings"
	"testing"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

func TestMissingEnv(t *testing.T) {
	t.Parallel()
	resume := strings.Join([]string{
		`uv pip install --index-url "$DEVPI_URL" -r req.txt`,
		`echo ${TOKEN_B} $TOKEN_B`,
		`echo ${WITH_DEFAULT:-x} ${D2-x} ${D3:=x} ${D4=x}`,
		`echo '$SINGLE_QUOTED' "$\"DQ_ESC"`,
		`echo $lower $HOME $PATH $GH_TOKEN $AWS_PROFILE $GIT_CONFIG_COUNT $BASH_SOURCE $1 $@ $? $$ $# $!`,
		`LOCAL_SET=1; export EXPORTED=2; read READ_V; for LOOP_V in a b; do :; done`,
		`f() { local LOCAL_V; echo $LOCAL_V $LOCAL_SET $EXPORTED $READ_V $LOOP_V; }`,
		`echo $IN_ENV $IN_SECRETS $HTTPS_PROXY $https_proxy $CI`,
	}, "\n")
	checks := projectdef.Checks{Fast: "make test FLAG=$FAST_V", Full: "echo ${FULL_V}"}
	def := projectdef.Def{
		Env:     map[string]string{"IN_ENV": "x"},
		Secrets: map[string]string{"IN_SECRETS": "keychain:bough/p/IN_SECRETS"},
	}
	got := missingEnv(resume, checks, def, "")
	want := []string{"DEVPI_URL", "FAST_V", "FULL_V", "TOKEN_B"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("missingEnv = %v, want %v", got, want)
	}
	if got := missingEnv("", projectdef.Checks{}, projectdef.Def{}, ""); len(got) != 0 {
		t.Errorf("empty = %v", got)
	}
}

func TestMissingEnvRealScripts(t *testing.T) {
	t.Parallel()
	resume := strings.Join([]string{
		`#!/bin/sh`,
		`# don't use $COMMENTED here`,
		`uv venv && source .venv/bin/activate`,
		`echo "$VIRTUAL_ENV" \$ESCAPED ${REQ:?set REQ} ${ALT:+x} ${#LEN}`,
		`pip install --index-url "$PIP_INDEX" .  # it's fine`,
		`go test ./... -tags $IMAGE_ENV $SETUP_EXPORT $STILL_MISSING`,
	}, "\n")
	image := "FROM x\nENV IMAGE_ENV=1\n" + "export SETUP_EXPORT=/opt\n"
	got := missingEnv(resume, projectdef.Checks{}, projectdef.Def{}, image)
	if want := []string{"PIP_INDEX", "STILL_MISSING"}; !reflect.DeepEqual(got, want) {
		t.Errorf("missingEnv = %v, want %v", got, want)
	}
}

func TestPromptSectionMissingEnvAndRule(t *testing.T) {
	t.Parallel()
	st := iorb.State{Project: "demo", Container: "c"}
	s := promptSection("/r", st, projectdef.Def{}, []string{"A", "B"})
	for _, want := range []string{
		"Env referenced by resume.sh/checks that may be unset: A, B. If so, set them (bough project set demo env.NAME / tools.secret) before trusting checks.",
		"do not fall back to weaker verification",
		"ask for secrets with `tools.secret`",
	} {
		if !strings.Contains(s, want) {
			t.Errorf("section lacks %q:\n%s", want, s)
		}
	}
	if s := promptSection("/r", st, projectdef.Def{}, nil); strings.Contains(s, "Unset env") {
		t.Errorf("no missing env still lists it:\n%s", s)
	}
}
