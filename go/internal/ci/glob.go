package ci

import (
	"path"
	"strings"
)

// matchGlob matches a repo-relative, /-separated path against pattern.
// "**" as a whole segment spans zero or more segments; every other
// segment is path.Match. Hand-rolled because nothing in the module
// matches "**" and it is not worth a dependency: the inputs of a check
// are nearly always "some/dir/**" or "*.lock".
func matchGlob(pattern, name string) bool {
	return matchSegs(strings.Split(pattern, "/"), strings.Split(name, "/"))
}

func matchSegs(pat, name []string) bool {
	for len(pat) > 0 {
		if pat[0] == "**" {
			rest := pat[1:]
			for i := 0; i <= len(name); i++ {
				if matchSegs(rest, name[i:]) {
					return true
				}
			}
			return false
		}
		if len(name) == 0 {
			return false
		}
		if ok, err := path.Match(pat[0], name[0]); err != nil || !ok {
			return false
		}
		pat, name = pat[1:], name[1:]
	}
	return len(name) == 0
}
