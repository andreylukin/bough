//go:build !unix

package orb

import "sync"

// No cross-process lock off unix: orbs have no runtime there yet, so an
// in-process mutex is enough to keep tests honest.
var buildMu sync.Mutex

func lockFile(string) (func(), error) {
	buildMu.Lock()
	return buildMu.Unlock, nil
}
