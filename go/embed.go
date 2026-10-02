// Package bough embeds the repo's default config tree so the binary
// can run from any directory with no bough.yml on disk.
package bough

import _ "embed"

//go:embed bough.yml
var DefaultConfig []byte

//go:embed skills/context-toolkit/SKILL.md
var contextToolkitSkill string

// BuiltinSkill serves immutable bundled instructions without materializing
// files in HOME. Only exact public skill identifiers are accepted.
func BuiltinSkill(path string) (string, bool) {
	if path == "builtin:context-toolkit" {
		return contextToolkitSkill, true
	}
	return "", false
}
