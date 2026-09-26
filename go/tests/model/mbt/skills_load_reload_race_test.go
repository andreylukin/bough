//go:build !windows

package mbt

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	control "github.com/andreylukin/bough/tests/model/llm"
)

// specs/skills_load_reload_race.fizz against a real serve. The skill is
// a file under HOME edited by the adapter; Invoke sends a prompt naming
// it, and the transcript's "[skill: name]" block says which version the
// child injected. The product rescans the pools on every input, so an
// invoke always sees the file as it is on disk.

const slrSkill = "rrfixture"

type skillsLoadReloadAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	path string
	gate gate

	id   string
	file string
	turn int

	// staleWrite is the deliberate bug: a write lands on disk as the
	// other version.
	staleWrite bool
}

func newSkillsLoadReloadAdapter(t *testing.T) *skillsLoadReloadAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &skillsLoadReloadAdapter{t: t, s: s, dir: control.Dir(s.Home),
		path: filepath.Join(s.Home, ".claude", "skills", slrSkill, "SKILL.md")}
}

func (a *skillsLoadReloadAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	if err := os.RemoveAll(filepath.Dir(a.path)); err != nil {
		return err
	}
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.file = row.ID, "absent"
	a.gate.reset()
	return nil
}

func (a *skillsLoadReloadAdapter) Cleanup() error { return nil }

func (a *skillsLoadReloadAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Skills", Index: 0}: a}, nil
}

func (a *skillsLoadReloadAdapter) skillInputs() ([]serve.Line, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	_, lines, err := a.s.GetSession(ctx, a.id)
	if err != nil {
		return nil, err
	}
	var out []serve.Line
	for _, l := range lines {
		if l.Kind == "input" && strings.Contains(l.Text, "\n[skill: "+slrSkill+"]") {
			out = append(out, l)
		}
	}
	return out, nil
}

func (a *skillsLoadReloadAdapter) GetState() (map[string]any, error) {
	ins, err := a.skillInputs()
	if err != nil {
		return nil, err
	}
	invoked := "none"
	if len(ins) > 0 {
		invoked = "v?"
		for _, v := range []string{"v1", "v2"} {
			if strings.Contains(ins[len(ins)-1].Text, "body "+v) {
				invoked = v
			}
		}
	}
	file := "absent"
	if b, err := os.ReadFile(a.path); err == nil {
		file = strings.TrimSpace(strings.TrimPrefix(string(b), "---\ndescription: fixture.\n---\nbody"))
	}
	return map[string]any{"file": file, "invoked": invoked, "updates": len(ins)}, nil
}

func (a *skillsLoadReloadAdapter) write(v string) error {
	if !a.gate.pass(a.file != v) {
		return nil
	}
	disk := v
	if a.staleWrite {
		disk = map[string]string{"v1": "v2", "v2": "v1"}[v]
	}
	if err := os.MkdirAll(filepath.Dir(a.path), 0o755); err != nil {
		return err
	}
	a.file = v
	return os.WriteFile(a.path, []byte("---\ndescription: fixture.\n---\nbody "+disk+"\n"), 0o644)
}

func (a *skillsLoadReloadAdapter) WriteV1() error { return a.write("v1") }
func (a *skillsLoadReloadAdapter) WriteV2() error { return a.write("v2") }

func (a *skillsLoadReloadAdapter) Remove() error {
	if !a.gate.pass(a.file != "absent") {
		return nil
	}
	a.file = "absent"
	return os.RemoveAll(filepath.Dir(a.path))
}

func (a *skillsLoadReloadAdapter) Invoke() error {
	if !a.gate.pass(a.file != "absent") {
		return nil
	}
	ins, err := a.skillInputs()
	if err != nil {
		return err
	}
	if !a.gate.pass(len(ins) < 2) {
		return nil
	}
	a.turn++
	name := fmt.Sprintf("s%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Text: "done " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "please use "+slrSkill+" "+name); err != nil {
		return err
	}
	_, err = waitRow(a.s, a.id, "the turn to end", func(r serve.Row) bool {
		return r.Status == serve.StatusDone
	})
	return err
}

var skillsLoadReloadActions = map[string]map[string]fmbt.ActionFunc{"Skills": {
	"WriteV1": action((*skillsLoadReloadAdapter).WriteV1),
	"WriteV2": action((*skillsLoadReloadAdapter).WriteV2),
	"Remove":  action((*skillsLoadReloadAdapter).Remove),
	"Invoke":  action((*skillsLoadReloadAdapter).Invoke),
}}

func skillsLoadReloadOptions() map[string]any {
	return map[string]any{"max-seq-runs": 60, "max-actions": 6, "max-parallel-runs": 0}
}

func TestSkillsLoadReloadRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSkillsLoadReloadAdapter(t)
	if err := runMBT(t, "skills_load_reload_race", a, skillsLoadReloadActions, skillsLoadReloadOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func TestSkillsLoadReloadRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSkillsLoadReloadAdapter(t)
	a.staleWrite = true
	if err := runMBT(t, "skills_load_reload_race", a, skillsLoadReloadActions, skillsLoadReloadOptions()); err == nil {
		t.Fatal("a run whose writes land as the wrong version passed")
	}
}
