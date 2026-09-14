package orb

import (
	"regexp"
	"sort"
	"strings"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

var (
	envRefRE    = regexp.MustCompile(`(\\?)\$(\{?)([A-Za-z_][A-Za-z0-9_]*)`)
	envDockerRE = regexp.MustCompile(`(?m)^\s*(?:ENV|ARG)\s+([A-Za-z_][A-Za-z0-9_]*)`)
	envAssignRE = regexp.MustCompile(`(?:^|[\s;&|(])(?:export\s+|local\s+|readonly\s+|declare\s+)?([A-Za-z_][A-Za-z0-9_]*)=`)
	envReadRE   = regexp.MustCompile(`\bread\s+(?:-\S+\s+)*([A-Za-z_][A-Za-z0-9_]*)`)
	envForRE    = regexp.MustCompile(`\bfor\s+([A-Za-z_][A-Za-z0-9_]*)\s+in\b`)
	envLocalRE  = regexp.MustCompile(`\blocal\s+([A-Za-z_][A-Za-z0-9_]*)`)
)

// providedEnv is what every exec already sets: core, proxy and shell.
var providedEnv = map[string]bool{}

// providedPrefixes is iorb's identity env (GIT_CONFIG_* from git config,
// IdentityEnvPrefixes) plus BASH_*, so the two lists never drift.
var providedPrefixes = append([]string{"GIT_CONFIG_", "BASH_"}, iorb.IdentityEnvPrefixes...)

func init() {
	for _, n := range projectdef.ReservedEnv {
		providedEnv[n] = true
	}
	for _, n := range strings.Fields(`PWD OLDPWD USER SHELL LANG LC_ALL TMPDIR HOSTNAME IFS PS1 RANDOM
		SECONDS LINENO UID EUID PPID OPTARG OPTIND CI`) {
		providedEnv[n] = true
	}
}

// missingEnv lists env names resume.sh or the checks reference that
// nothing sets: not the definition's env or secrets, not what every
// exec provides, not assigned in the script or the image (setup.sh,
// Dockerfile ENV/ARG), not defaulted. It is a hint: tools the script
// runs can set more.
func missingEnv(resume string, checks projectdef.Checks, def projectdef.Def, image string) []string {
	src := stripSingleQuoted(resume + "\n" + checks.Fast + "\n" + checks.Full)
	img := stripSingleQuoted(image)
	assigned := map[string]bool{}
	for _, re := range []*regexp.Regexp{envAssignRE, envReadRE, envForRE, envLocalRE, envDockerRE} {
		for _, m := range re.FindAllStringSubmatch(src+"\n"+img, -1) {
			assigned[m[1]] = true
		}
	}
	if strings.Contains(src+img, "bin/activate") {
		assigned["VIRTUAL_ENV"] = true
	}
	seen := map[string]bool{}
	var out []string
	for _, m := range envRefRE.FindAllStringSubmatchIndex(src, -1) {
		name := src[m[6]:m[7]]
		if m[3] > m[2] { // \$NAME is literal
			continue
		}
		if m[5] > m[4] && hasDefault(src[m[7]:]) {
			continue
		}
		if seen[name] || name == strings.ToLower(name) || assigned[name] || providedEnv[name] || provided(name, def) {
			continue
		}
		seen[name] = true
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}

func hasDefault(rest string) bool {
	for _, p := range []string{":-", "-", ":=", "=", ":?", "?", ":+", "+"} {
		if strings.HasPrefix(rest, p) {
			return true
		}
	}
	return false
}

func provided(name string, def projectdef.Def) bool {
	if _, ok := def.Env[name]; ok {
		return true
	}
	if _, ok := def.Secrets[name]; ok {
		return true
	}
	for _, p := range providedPrefixes {
		if strings.HasPrefix(name, p) {
			return true
		}
	}
	return false
}

// stripSingleQuoted blanks '...' spans outside double quotes, where
// the shell expands nothing, and # comments outside quotes, so an
// apostrophe in a comment opens no span.
func stripSingleQuoted(s string) string {
	var b strings.Builder
	single, double := false, false
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case !single && !double && c == '#' && (i == 0 || strings.IndexByte(" \t\n;&|(", s[i-1]) >= 0):
			for i < len(s) && s[i] != '\n' {
				i++
			}
			if i < len(s) {
				b.WriteByte('\n')
			}
			continue
		case single:
			if c == '\'' {
				single = false
			}
			continue
		case c == '\\' && i+1 < len(s):
			b.WriteByte(c)
			i++
			c = s[i]
		case c == '"':
			double = !double
		case c == '\'' && !double:
			single = true
			continue
		}
		b.WriteByte(c)
	}
	return b.String()
}
