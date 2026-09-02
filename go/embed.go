// Package bough embeds the repo's default config tree so the binary
// can run from any directory with no bough.yml on disk.
package bough

import _ "embed"

//go:embed bough.yml
var DefaultConfig []byte
