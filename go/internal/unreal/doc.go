// Package unreal holds the engine's pin of unreal-agent; its
// subpackages adapt the harness to bough (go/docs/unreal-engine.md §3).
//
// The pin is a SHA, not a range: the harness is days old, its store
// format has already broken once, and the contract tests in
// internal/unreal/contract pin behaviour its README does not promise.
// A bump is its own PR (§16 upgrade checklist).
package unreal

// Pin and PinSHA are what the `engine` history entry records, so a
// session names the harness it was written with. pin_test.go fails when
// go.mod moves without them.
const (
	Pin    = "v0.1.1"
	PinSHA = "b7c9bf1c5c2fa4127255c07727a7c8413e23944a"
)

// Module is the harness module path.
const Module = "github.com/unreallabsai/unreal-agent"
