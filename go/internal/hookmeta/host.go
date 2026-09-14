package hookmeta

import "context"

// Hooks are the user's own scripts, not project code: in a project
// session their shell commands run on the host, where $HOME and the
// user's files are, never inside the orb.

type hostKey struct{}

// WithHost marks ctx as a hook run.
func WithHost(ctx context.Context) context.Context {
	return context.WithValue(ctx, hostKey{}, true)
}

// OnHost reports whether ctx is a hook run.
func OnHost(ctx context.Context) bool {
	v, _ := ctx.Value(hostKey{}).(bool)
	return v
}
