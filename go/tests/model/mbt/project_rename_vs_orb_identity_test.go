//go:build !windows

package mbt

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
)

// specs/project_rename_vs_orb_identity.fizz against a real serve: no
// sessions, no orb runtime work — the flow is entirely about
// supervisor.RenameProject (splices project.yml's name: line only) and
// the fact that everything orb-shaped keys off the slug, never off that
// name. serve runs in process (serve.NewSupervisor + container.Fake, as
// orb_lifecycle does), because a `bough serve` process always reaches
// for the host's container runtime.
//
// orb_building/orb_running are adapter bookkeeping (this flow has no
// build to race, only the fact that a rename can happen at any point in
// their lifecycle without moving them, which the product guarantees by
// construction: RenameProject touches nothing but the name line).
// orb_identity is not bookkeeping: it is the real tag a build would use
// right now, computed with the product's own projectdef.ImageHash and
// projectdef.ImageTag off the project's slug and definition on disk, so
// a rename that ever leaked into the image tag would show up here.

type pvoAdapter struct {
	t    *testing.T
	sup  *serve.Supervisor
	home string
	gate gate

	n                   int
	slug                string
	alphaName, betaName string
	uiName              string // the page's own cached read, moved only by UiPoll
	building, running   bool
	baseline            string // the tag Init captured, before any rename

	// wrongIdentity is the deliberate wiring bug
	// TestProjectRenameVsOrbIdentityCatchesWrongAdapter injects: the
	// adapter reports the tag a build would use if the image were keyed
	// by the display name instead of the slug, the bug the spec's
	// comments say the product does not have.
	wrongIdentity bool
}

func newPVOAdapter(t *testing.T) *pvoAdapter {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	if err := os.MkdirAll(filepath.Join(home, ".bough", "history"), 0o755); err != nil {
		t.Fatal(err)
	}
	sup, err := serve.NewSupervisor(serve.Options{
		HistDir:  filepath.Join(home, ".bough", "history"),
		MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"),
		Runtime:  container.NewFake(),
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	return &pvoAdapter{t: t, sup: sup, home: home}
}

// Init starts each walk on a fresh project named Alpha<n>, its slug the
// only thing every later step keys off of.
func (a *pvoAdapter) Init() error {
	a.n++
	a.alphaName = fmt.Sprintf("Alpha%d", a.n)
	a.betaName = fmt.Sprintf("Beta%d", a.n)
	p, err := a.sup.NewProject(a.alphaName)
	if err != nil {
		return err
	}
	a.slug = p.Slug
	a.uiName = "Alpha"
	a.building, a.running = false, false
	proj, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return err
	}
	hash, err := projectdef.ImageHash(a.home, proj)
	if err != nil {
		return err
	}
	a.baseline = projectdef.ImageTag(a.slug, hash)
	a.gate.reset()
	return nil
}

func (a *pvoAdapter) Cleanup() error { return nil }

func (a *pvoAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Project", Index: 0}: a}, nil
}

// diskName is project.yml's name: line, read the way GET
// /api/projects/{slug} reads it (Supervisor.Project).
func (a *pvoAdapter) diskName() (string, error) {
	p, ok := a.sup.Project(a.slug)
	if !ok {
		return "", fmt.Errorf("project %s vanished", a.slug)
	}
	return p.Name, nil
}

func (a *pvoAdapter) classifyName(name string) string {
	switch name {
	case a.alphaName:
		return "Alpha"
	case a.betaName:
		return "Beta"
	default:
		return "other:" + name
	}
}

// pvoFakeSlugify stands in for serve's slugify, only for the deliberate
// bug below: it need not match it exactly, only differ from a.slug once
// the name has changed.
func pvoFakeSlugify(name string) string {
	return strings.ToLower(name)
}

// identity is the tag EnsureImage would use for a build started right
// now, or, under the injected bug, the tag a build keyed by the current
// display name would use instead of the slug. It is classified against
// the baseline Init captured: "alpha-slug" when nothing has moved it,
// the differing tag itself otherwise (which the spec's literal never
// equals, so a real drift fails the walk instead of comparing quietly).
func (a *pvoAdapter) identity() (string, error) {
	proj, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return "", err
	}
	hash, err := projectdef.ImageHash(a.home, proj)
	if err != nil {
		return "", err
	}
	slug := a.slug
	if a.wrongIdentity {
		name, err := a.diskName()
		if err != nil {
			return "", err
		}
		slug = pvoFakeSlugify(name)
	}
	tag := projectdef.ImageTag(slug, hash)
	if tag == a.baseline {
		return "alpha-slug", nil
	}
	return tag, nil
}

type pvoView struct {
	yName, uiName     string
	building, running bool
	identity          string
}

func (a *pvoAdapter) view() (pvoView, error) {
	name, err := a.diskName()
	if err != nil {
		return pvoView{}, err
	}
	id, err := a.identity()
	if err != nil {
		return pvoView{}, err
	}
	return pvoView{yName: a.classifyName(name), uiName: a.uiName, building: a.building, running: a.running, identity: id}, nil
}

func (a *pvoAdapter) state(v pvoView) map[string]any {
	return map[string]any{
		"y_name": v.yName, "ui_name": v.uiName,
		"orb_building": v.building, "orb_running": v.running,
		"orb_identity": v.identity,
	}
}

func (a *pvoAdapter) GetState() (map[string]any, error) {
	v, err := a.view()
	if err != nil {
		return nil, err
	}
	return a.state(v), nil
}

// step gates an action on the spec's require, read off the current view.
func (a *pvoAdapter) step(require func(pvoView) bool, do func(pvoView) error) error {
	v, err := a.view()
	if err != nil {
		return err
	}
	if !a.gate.pass(require(v)) {
		return nil
	}
	return do(v)
}

// Rename is RenameProject -> SetName: it splices only the name: line, at
// any point in the orb's lifecycle.
func (a *pvoAdapter) Rename() error {
	return a.step(func(v pvoView) bool { return v.yName == "Alpha" }, func(pvoView) error {
		return a.sup.RenameProject(a.slug, a.betaName)
	})
}

// UiPoll is the open page's own re-read of the project's name: it only
// ever catches up to whatever RenameProject last wrote.
func (a *pvoAdapter) UiPoll() error {
	return a.step(func(v pvoView) bool { return v.uiName != v.yName }, func(pvoView) error {
		name, err := a.diskName()
		if err != nil {
			return err
		}
		a.uiName = a.classifyName(name)
		return nil
	})
}

func (a *pvoAdapter) StartOrbBuild() error {
	return a.step(func(v pvoView) bool { return !v.building }, func(pvoView) error {
		a.building = true
		return nil
	})
}

func (a *pvoAdapter) FinishOrbBuild() error {
	return a.step(func(v pvoView) bool { return v.building }, func(pvoView) error {
		a.building, a.running = false, true
		return nil
	})
}

func (a *pvoAdapter) StopOrb() error {
	return a.step(func(v pvoView) bool { return v.running }, func(pvoView) error {
		a.running = false
		return nil
	})
}

var pvoActions = map[string]map[string]fmbt.ActionFunc{"Project": {
	"Rename":         action((*pvoAdapter).Rename),
	"UiPoll":         action((*pvoAdapter).UiPoll),
	"StartOrbBuild":  action((*pvoAdapter).StartOrbBuild),
	"FinishOrbBuild": action((*pvoAdapter).FinishOrbBuild),
	"StopOrb":        action((*pvoAdapter).StopOrb),
}}

// Every step here is in-process (no session, no child, no real
// container), so a walk is fast; still bounded like the example.
func pvoOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 10, "max-parallel-runs": 0}
}

func TestProjectRenameVsOrbIdentity(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPVOAdapter(t)
	if err := runMBT(t, "project_rename_vs_orb_identity", a, pvoActions, pvoOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a server that breaks the model
// fails it. This adapter reports the tag a name-keyed build would use
// instead of the real slug-keyed one, the bug the spec's OrbIdentityFixed
// assertion exists to catch, and the run must fail once a walk renames.
func TestProjectRenameVsOrbIdentityCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPVOAdapter(t)
	a.wrongIdentity = true
	if err := runMBT(t, "project_rename_vs_orb_identity", a, pvoActions, pvoOptions()); err == nil {
		t.Fatal("a run whose identity follows the display name passed; the walk is not checking state")
	}
}
