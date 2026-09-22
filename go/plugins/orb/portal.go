package orb

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strconv"

	"github.com/andreylukin/bough/internal/agenttools"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/kernel"
)

// toolRegistry is codemode, declared structurally so this package does
// not import it (the example plugin's pattern).
type toolRegistry interface {
	RegisterTool(name string, fn any)
	Describe(name, line string)
}

// errPortalNotReady is tools-basic's refusal, word for word: a portal
// before the orb is up has nothing to reach.
var errPortalNotReady = errors.New("project orb not ready: see the orb row")

// registerPortalTools binds tools.portal for a project session, and the
// native portal tool. Portals are per-session, so the listeners close
// with the row.
func registerPortalTools(ctx *kernel.Context, home, session string) {
	open := func(port int, name string) (map[string]any, error) {
		ps, err := iorb.OpenPortal(home, session, port, name)
		if err != nil {
			return nil, err
		}
		return portalJSON(ps), nil
	}
	list := func() ([]map[string]any, error) {
		st, err := iorb.ReadState(home, session)
		if err != nil {
			return nil, fmt.Errorf("portal: %w", err)
		}
		out := make([]map[string]any, 0, len(st.Portals))
		for _, p := range st.Portals {
			out = append(out, portalJSON(p))
		}
		return out, nil
	}
	closePortal := func(port int) (map[string]any, error) {
		if err := iorb.ClosePortal(home, session, port); err != nil {
			return nil, err
		}
		return map[string]any{"closed": port}, nil
	}
	// Registered at Apply whether or not the orb is up yet, so the
	// engine's tool set never changes when it comes up: a change there
	// restarts the coordinator with a new tools block.
	if at, err := kernel.Get[agenttools.Registry](ctx, "agent-tools"); err == nil {
		ready := func() error {
			o, err := kernel.Get[interface{ Starting() bool }](ctx, "orb")
			if err != nil || o.Starting() {
				return errPortalNotReady
			}
			return nil
		}
		if off, err := at.Register(nativePortal(ready, open, list, closePortal)); err == nil {
			ctx.Effect(off)
		} else {
			fmt.Fprintf(os.Stderr, "bough: orb: native portal: %v\n", err)
		}
	}
	reg, err := kernel.Get[toolRegistry](ctx, "codemode")
	if err != nil {
		ctx.Effect(func() { iorb.CloseSessionPortals(home, session) })
		return // no code mode in this session: nothing to bind to
	}
	reg.RegisterTool("portal", map[string]any{
		"open":  open,
		"list":  list,
		"close": closePortal,
	})
	reg.Describe("portal", "portal.open(guestPort, name?) shows a server running in the orb at a loopback URL the user can open; portal.list(); portal.close(guestPort)")
	ctx.Effect(func() {
		reg.RegisterTool("portal", nil)
		reg.Describe("portal", "")
		iorb.CloseSessionPortals(home, session)
	})
}

// nativePortal is portal.open/list/close as one native tool; ready
// refuses every op while the orb is not up.
func nativePortal(ready func() error, open func(int, string) (map[string]any, error), list func() ([]map[string]any, error), closePortal func(int) (map[string]any, error)) agenttools.Tool {
	type args struct {
		Op    string `json:"op"`
		Port  int    `json:"port"`
		Label string `json:"label"`
	}
	return agenttools.Tool{
		Name:        "portal",
		Description: "Show a server running in the orb to the user at a loopback URL they can open. op open (port, optional label) is the default; list shows the open portals; close (port) closes one.",
		Schema: agenttools.Object(nil, map[string]any{
			"op":    map[string]any{"type": "string", "enum": []any{"open", "list", "close"}},
			"port":  agenttools.Prop("integer", "the port the server listens on inside the orb"),
			"label": agenttools.Prop("string", "a name for the user"),
		}),
		Detail: func(raw json.RawMessage) string {
			var a args
			_ = json.Unmarshal(raw, &a)
			switch a.Op {
			case "list":
				return "list"
			case "":
				a.Op = "open"
			}
			return a.Op + " " + strconv.Itoa(a.Port)
		},
		Call: func(_ context.Context, c agenttools.Call) (agenttools.Result, error) {
			var a args
			if err := agenttools.Decode("portal", c.Args, &a); err != nil {
				return agenttools.Result{}, err
			}
			if err := ready(); err != nil {
				return agenttools.Result{Error: "portal: " + err.Error()}, nil
			}
			var out any
			var err error
			switch a.Op {
			case "", "open":
				out, err = open(a.Port, a.Label)
			case "list":
				out, err = list()
			case "close":
				out, err = closePortal(a.Port)
			default:
				err = fmt.Errorf("portal: op must be open, list or close, got %q", a.Op)
			}
			if err != nil {
				return agenttools.Result{Error: err.Error()}, nil
			}
			b, _ := json.Marshal(out)
			return agenttools.Result{Text: string(b)}, nil
		},
	}
}

func portalJSON(p iorb.PortalState) map[string]any {
	m := map[string]any{"guest": p.Guest, "host": p.Host, "url": p.URL()}
	if p.Name != "" {
		m["name"] = p.Name
	}
	return m
}
