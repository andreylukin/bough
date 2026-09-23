//go:build !windows

package session

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore"
	"github.com/unreallabsai/unreal-agent/harness/tool"
	"github.com/unreallabsai/unreal-agent/harness/tool/viewimage"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

// ReadGuard is the optional slice of turn-stats (tools-basic's Stats)
// that confines what a project session may read: the orb root, the
// scratchpad and the project's own directory, as its view does.
type ReadGuard interface {
	ReadAllowed(path string) error
}

// guardedView is the harness view_image translator behind bough's own
// rules. view_image is a harness op, not a bough.call, so the project
// session's read confinement and the pre-code-exec hook never ran for
// it: its config only prefixes a relative path, and an absolute one, or
// one with .., loaded any image on the host.
type guardedView struct {
	tool.Translator
	dir   string
	allow func(path string) error
	hooks agenttools.Hooks
}

func (r *Runtime) viewImage(dir string) tool.Translator {
	g := guardedView{Translator: viewimage.New(viewimage.Config{Directory: dir}), dir: dir, hooks: lazyHooks{r}}
	g.allow = func(path string) error {
		if r.d.Stats == nil {
			return nil
		}
		if rg, ok := r.d.Stats().(ReadGuard); ok {
			return rg.ReadAllowed(path)
		}
		return nil
	}
	return g
}

func (g guardedView) Translate(ctx tool.Context, call ullm.ToolCall) tool.CallStatus {
	var a struct {
		Path string `json:"path"`
	}
	if json.Unmarshal([]byte(call.Arguments), &a) != nil || strings.TrimSpace(a.Path) == "" {
		return g.Translator.Translate(ctx, call) // its own error says what is wrong
	}
	if g.hooks != nil {
		args, deny := g.hooks.PreTool(context.Background(), toolreg.ViewImageName, agenttools.Call{ID: call.CallID, Args: json.RawMessage(call.Arguments)}, a.Path)
		if deny != "" {
			return tool.CallStatus{Error: "blocked by hook: " + deny}
		}
		if args != nil {
			call.Arguments = string(args)
			if json.Unmarshal(args, &a) != nil {
				return g.Translator.Translate(ctx, call)
			}
		}
	}
	if err := g.check(a.Path); err != nil {
		return tool.CallStatus{Error: err.Error()}
	}
	return g.Translator.Translate(ctx, call)
}

// check resolves the path the way the harness will read it and asks the
// session's read guard.
func (g guardedView) check(path string) error {
	if g.allow == nil {
		return nil
	}
	if !filepath.IsAbs(path) && g.dir != "" {
		path = filepath.Join(g.dir, path)
	}
	return g.allow(path)
}

// viewedImage fires post-result for a view_image call that just ended,
// so the hook ledger sees every native call as hooks.md says. What the
// model reads is an image, so a rewrite is not applied.
func (a *actorState) viewedImage(st sessionstore.ToolCallStatus) {
	tool, detail, ok := a.proj.Call(st.CallID)
	if !ok || tool != toolreg.ViewImageName || a.r.d.Hooks == nil {
		return
	}
	h := a.r.d.Hooks()
	if h == nil {
		return
	}
	res := agenttools.Result{Error: st.Status.Error}
	for _, op := range st.Operations {
		if s := string(op.Status); s == "failed" || s == "canceled" {
			res.Error = strings.TrimSpace(res.Error + " view_image " + s)
		}
	}
	if res.Error == "" {
		res.Text = "image " + detail
	}
	args, _ := json.Marshal(map[string]string{"path": detail})
	go h.PostTool(a.r.ctx, tool, agenttools.Call{ID: st.CallID, Args: args}, detail, res)
}
