package orb

import (
	"fmt"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/kernel"
)

// toolRegistry is codemode, declared structurally so this package does
// not import it (the example plugin's pattern).
type toolRegistry interface {
	RegisterTool(name string, fn any)
	Describe(name, line string)
}

// registerPortalTools binds tools.portal for a project session. Portals
// are per-session, so the listeners close with the row.
func registerPortalTools(ctx *kernel.Context, home, session string) {
	reg, err := kernel.Get[toolRegistry](ctx, "codemode")
	if err != nil {
		return // no code mode in this session: nothing to bind to
	}
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

func portalJSON(p iorb.PortalState) map[string]any {
	m := map[string]any{"guest": p.Guest, "host": p.Host, "url": p.URL()}
	if p.Name != "" {
		m["name"] = p.Name
	}
	return m
}
