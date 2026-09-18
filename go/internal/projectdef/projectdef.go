// Package projectdef reads and writes project definitions: the
// ~/.bough/projects/<slug>/ directory holding project.yml plus a
// Dockerfile or setup.sh and an optional resume.sh. Definitions live
// outside every repo on purpose — they describe the user's machine
// setup, not the code, and are never committed.
package projectdef

import (
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strconv"
	"strings"
	"unicode"

	"gopkg.in/yaml.v3"
)

type Repo struct {
	Remote string `yaml:"remote,omitempty"` // git URL; one of Remote/Path
	Path   string `yaml:"path,omitempty"`   // local checkout on the host, ~ expanded
	Branch string `yaml:"branch,omitempty"` // base branch; "" = remote HEAD / current
	Name   string `yaml:"name,omitempty"`   // worktree dir name; "" = basename
}

type Checks struct {
	Fast string `yaml:"fast,omitempty"`
	Full string `yaml:"full,omitempty"`
}

type Def struct {
	Repos  []Repo            `yaml:"repos"`
	Checks Checks            `yaml:"checks,omitempty"`
	LSP    []string          `yaml:"lsp,omitempty"`    // roots; parsed, unused this run
	Base   string            `yaml:"base,omitempty"`   // setup-script base; "" = bough's base image (BaseTag)
	Caches []string          `yaml:"caches,omitempty"` // guest dirs bound to host dirs under ~/.bough/cache/<slug>
	Env    map[string]string `yaml:"env,omitempty"`
	// Secrets maps an env name to a ref (keychain:<service>); values never
	// live in this file.
	Secrets map[string]string `yaml:"secrets,omitempty"`
	// Redact replaces resolved secret values (8+ chars) in tool output,
	// history and resume.log; nil = on, `redact: false` opts out.
	Redact *bool `yaml:"redact,omitempty"`
	// Identity opts the container into the user's host identity; nothing
	// is lent by default. "<dir>" mounts $HOME/<dir> read-only at
	// /root/<dir>, "<dir>:rw" read-write, and "gh" passes GH_TOKEN.
	Identity []string `yaml:"identity,omitempty"`
	CPUs     int      `yaml:"cpus,omitempty"`
	Memory   string   `yaml:"memory,omitempty"`
	// Ports opts into host forwards: each is published on 127.0.0.1 only.
	// A host port already in use is skipped, not fatal. The container's own
	// IP always reaches every port.
	Ports []Port `yaml:"ports,omitempty"`
}

// Port forwards 127.0.0.1:Host on the host to Guest in the container.
// In YAML it is 3000 (same port both sides) or "8080:80" (host:guest).
type Port struct{ Host, Guest int }

func (p Port) String() string {
	if p.Host == p.Guest {
		return strconv.Itoa(p.Host)
	}
	return fmt.Sprintf("%d:%d", p.Host, p.Guest)
}

func (p Port) MarshalYAML() (any, error) {
	if p.Host == p.Guest {
		return p.Host, nil
	}
	return p.String(), nil
}

func (p *Port) UnmarshalYAML(n *yaml.Node) error {
	q, err := parsePort(n.Value)
	if err != nil {
		return fmt.Errorf("ports: %w", err)
	}
	*p = q
	return nil
}

func parsePort(s string) (Port, error) {
	num := func(v string) (int, error) {
		n, err := strconv.Atoi(strings.TrimSpace(v))
		if err != nil || n < 1 || n > 65535 {
			return 0, fmt.Errorf("%q is not a port (want 1-65535, or host:guest like 8080:80)", s)
		}
		return n, nil
	}
	h, g, pair := strings.Cut(s, ":")
	host, err := num(h)
	if err != nil {
		return Port{}, err
	}
	if !pair {
		return Port{Host: host, Guest: host}, nil
	}
	guest, err := num(g)
	if err != nil {
		return Port{}, err
	}
	return Port{Host: host, Guest: guest}, nil
}

// ParsePorts reads a comma-separated list ("3000, 8080:80"); "" is none.
func ParsePorts(s string) ([]Port, error) {
	var ports []Port
	for f := range strings.SplitSeq(s, ",") {
		if strings.TrimSpace(f) == "" {
			continue
		}
		p, err := parsePort(strings.TrimSpace(f))
		if err != nil {
			return nil, err
		}
		ports = append(ports, p)
	}
	return ports, checkPorts(ports)
}

func checkPorts(ports []Port) error {
	seen := map[int]bool{}
	for i, p := range ports {
		if seen[p.Host] {
			return fmt.Errorf("ports[%d]: host port %d is listed twice", i, p.Host)
		}
		seen[p.Host] = true
	}
	return nil
}

// Project is one definition on disk.
type Project struct {
	Slug string // directory name: [a-z0-9][a-z0-9-]{0,62}
	Dir  string // ~/.bough/projects/<slug>
	Def  Def
}

const (
	FileYAML       = "project.yml"
	FileDockerfile = "Dockerfile"
	FileSetup      = "setup.sh"
	FileResume     = "resume.sh"
)

var EditableFiles = []string{FileYAML, FileDockerfile, FileSetup, FileResume}

var slugRE = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,62}$`)

func Root(home string) string { return filepath.Join(home, ".bough", "projects") }

func ValidSlug(s string) error {
	if !slugRE.MatchString(s) {
		return fmt.Errorf("projectdef: bad slug %q (want [a-z0-9][a-z0-9-]{0,62})", s)
	}
	return nil
}

// Parse decodes and validates project.yml. Unknown keys are rejected so a
// typo ("repo:" for "repos:") fails loudly instead of building nothing.
func Parse(b []byte) (Def, error) {
	var d Def
	var probe struct {
		CPUs any `yaml:"cpus"`
	}
	if yaml.Unmarshal(b, &probe) == nil && probe.CPUs != nil {
		if _, ok := probe.CPUs.(int); !ok {
			return Def{}, fmt.Errorf("projectdef: %s: cpus: %v is not a whole number", FileYAML, probe.CPUs)
		}
	}
	dec := yaml.NewDecoder(strings.NewReader(string(b)))
	dec.KnownFields(true)
	if err := dec.Decode(&d); err != nil {
		return Def{}, fmt.Errorf("projectdef: parse %s: %w", FileYAML, err)
	}
	if len(d.Repos) == 0 {
		return Def{}, fmt.Errorf("projectdef: %s: at least one repo is required", FileYAML)
	}
	seen := map[string]bool{}
	for i, r := range d.Repos {
		if (r.Remote == "") == (r.Path == "") {
			return Def{}, fmt.Errorf("projectdef: %s: repos[%d]: set exactly one of remote or path", FileYAML, i)
		}
		n := r.RepoName()
		if n == "" || n == "." || n == ".." || strings.ContainsAny(n, `/\`) || n == "cache" {
			return Def{}, fmt.Errorf("projectdef: %s: repos[%d]: bad name %q", FileYAML, i, n)
		}
		if seen[n] {
			return Def{}, fmt.Errorf("projectdef: %s: repos[%d]: duplicate name %q (set name:)", FileYAML, i, n)
		}
		seen[n] = true
	}
	if err := checkPorts(d.Ports); err != nil {
		return Def{}, fmt.Errorf("projectdef: %s: %w", FileYAML, err)
	}
	if d.CPUs < 0 {
		return Def{}, fmt.Errorf("projectdef: %s: cpus must be >= 0", FileYAML)
	}
	for _, name := range slices.Sorted(maps.Keys(d.Secrets)) {
		if err := checkSecret(name, d.Secrets[name]); err != nil {
			return Def{}, fmt.Errorf("projectdef: %s: %w", FileYAML, err)
		}
		if _, ok := d.Env[name]; ok {
			return Def{}, fmt.Errorf("projectdef: %s: secrets.%s: also set in env", FileYAML, name)
		}
	}
	for _, name := range slices.Sorted(maps.Keys(d.Env)) {
		if reservedEnv(name) {
			return Def{}, fmt.Errorf("projectdef: %s: env.%s: reserved env name (set by every exec)", FileYAML, name)
		}
	}
	for _, dir := range d.Identity {
		if err := CheckIdentity(dir); err != nil {
			return Def{}, fmt.Errorf("projectdef: %s: identity: %w", FileYAML, err)
		}
	}
	return d, nil
}

// memoryRE is a size the container runtime's -m flag takes: a whole
// number with an optional K/M/G/T/P unit, which may be written the IEC
// way — 8G, 512MB, 8GiB. It was narrower than the runtime: `container`
// takes 8GiB and allocates exactly 8 GiB for it, but a project.yml
// carrying that size failed validation, and since `bough project set`
// validates before it writes, every later edit to that project was
// refused over a value the runtime was perfectly happy with.
var memoryRE = regexp.MustCompile(`^[1-9][0-9]*([KkMmGgTtPp][Ii]?)?[Bb]?$`)

// Placeholder is the example repo path in a new project's skeleton.
const Placeholder = "~/repos/example"

// Invalid lists what is wrong with a project.yml, in words for a person:
// the CLI and the web editor show Error() as it stands.
type Invalid struct{ Problems []string }

func (e *Invalid) Error() string {
	if len(e.Problems) == 1 {
		return FileYAML + ": " + e.Problems[0]
	}
	return fmt.Sprintf("%s has %d problems:\n  %s", FileYAML, len(e.Problems), strings.Join(e.Problems, "\n  "))
}

// CheckHost checks d against this machine: local repo paths are
// directories (and not the skeleton's placeholder), memory is a size,
// and identity dirs exist under home. Only writes run it; Load does not,
// so a definition made elsewhere still lists.
func CheckHost(home string, d Def) error {
	var probs []string
	for i, r := range d.Repos {
		if r.Path == "" {
			continue
		}
		if r.Path == Placeholder {
			probs = append(probs, fmt.Sprintf("repos[%d].path: %s is the template placeholder; set it to your checkout (a local path) or use remote:", i, r.Path))
			continue
		}
		if st, err := os.Stat(ExpandPath(home, r.Path)); err != nil {
			probs = append(probs, fmt.Sprintf("repos[%d].path: %s does not exist", i, r.Path))
		} else if !st.IsDir() {
			probs = append(probs, fmt.Sprintf("repos[%d].path: %s is not a directory", i, r.Path))
		}
	}
	if d.Memory != "" && !memoryRE.MatchString(d.Memory) {
		probs = append(probs, fmt.Sprintf("memory: %q is not a size (want a number with K, M, G or T, like 8G)", d.Memory))
	}
	for _, e := range d.Identity {
		dir, _ := IdentityDir(e)
		if dir == "" {
			continue
		}
		if st, err := os.Stat(filepath.Join(home, dir)); err != nil || !st.IsDir() {
			probs = append(probs, fmt.Sprintf("identity: ~/%s does not exist on this machine (log in to that CLI first, or remove it)", dir))
		}
	}
	if len(probs) > 0 {
		return &Invalid{Problems: probs}
	}
	return nil
}

var envNameRE = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

// ReservedEnv is what every orb exec sets itself (core, identity, proxy);
// neither a secret nor env may shadow it. BOUGH_ and GIT_CONFIG_ are reserved as
// prefixes.
var ReservedEnv = []string{"HOME", "TERM", "PATH", "BOUGH_SCRATCH", "BOUGH_HOST", "GH_TOKEN",
	"HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
	"SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS", "REQUESTS_CA_BUNDLE",
	"AWS_PROFILE", "AWS_REGION", "AWS_DEFAULT_REGION"}

func reservedEnv(name string) bool {
	return slices.Contains(ReservedEnv, name) || strings.HasPrefix(name, "BOUGH_") || strings.HasPrefix(name, "GIT_CONFIG_")
}

// IdentityGitHub is the identity entry that passes the host's GitHub
// token (gh auth token) as GH_TOKEN.
const IdentityGitHub = "gh"

// IdentityDir splits a dir identity entry into its $HOME-relative dir
// and whether it is read-write (":rw"); "gh" yields no dir.
func IdentityDir(entry string) (dir string, rw bool) {
	if entry == IdentityGitHub {
		return "", false
	}
	dir, rw = strings.CutSuffix(entry, ":rw")
	return dir, rw
}

// CheckIdentity accepts "gh" or a clean $HOME-relative dir, optionally
// suffixed ":rw", that is not a key store or bough's own state: a project
// may lend its container a CLI's login, never the user's SSH or GPG keys,
// nor ~/.bough (definitions, keys, other sessions), nor a dir the host
// runs code or reads tokens from (Library/LaunchAgents, ~/.local/bin,
// ~/.config itself or its shell/git/gh config): a writable mount there
// would let the container run code on the host.
func CheckIdentity(entry string) error {
	if entry == IdentityGitHub {
		return nil
	}
	dir, _ := IdentityDir(entry)
	if dir == "" || strings.Contains(dir, ":") || filepath.IsAbs(dir) || strings.HasPrefix(dir, "~") || filepath.Clean(dir) != dir || dir == "." || dir == ".." || strings.HasPrefix(dir, "../") {
		return fmt.Errorf("%q: want gh, or a clean path relative to $HOME like .circleci (add :rw for read-write)", entry)
	}
	top := strings.SplitN(dir, "/", 2)[0]
	if slices.Contains([]string{".ssh", ".gnupg", ".bough", "Library", ".local", ".docker"}, top) {
		return fmt.Errorf("%q: %s is never mounted into a project container", entry, top)
	}
	parts := strings.SplitN(dir, "/", 3)
	if top == ".config" && (len(parts) == 1 || slices.Contains([]string{"fish", "git", "gh", "zsh", "bash", "nvim", "autostart", "systemd", "launchd"}, parts[1])) {
		return fmt.Errorf("%q: mount a CLI's own dir under .config (like .config/gcloud), not shell, git or gh config", entry)
	}
	return nil
}

func checkSecret(name, ref string) error {
	if !envNameRE.MatchString(name) {
		return fmt.Errorf("secrets.%s: bad env name", name)
	}
	if reservedEnv(name) {
		return fmt.Errorf("secrets.%s: reserved env name (set by every exec)", name)
	}
	scheme, service, ok := strings.Cut(ref, ":")
	if !ok || scheme != "keychain" {
		return fmt.Errorf("secrets.%s: unknown ref scheme %q (want keychain:)", name, scheme)
	}
	if service == "" || len(service) > 200 || strings.IndexFunc(service, func(r rune) bool { return unicode.IsSpace(r) || unicode.IsControl(r) }) >= 0 {
		return fmt.Errorf("secrets.%s: bad keychain service %q", name, service)
	}
	return nil
}

// SetSecret points name at ref in the project's secrets, dropping an env
// entry of the same name, and writes project.yml through WriteFile.
func SetSecret(home, slug, name, ref string) error {
	p, err := Load(home, slug)
	if err != nil {
		return err
	}
	if p.Def.Secrets == nil {
		p.Def.Secrets = map[string]string{}
	}
	p.Def.Secrets[name] = ref
	delete(p.Def.Env, name)
	b, err := yaml.Marshal(p.Def)
	if err != nil {
		return fmt.Errorf("projectdef: set secret %s: %w", name, err)
	}
	// A secret ref is not a host edit: skip CheckHost so a fresh skeleton
	// (placeholder repo) or a vanished repo does not block storing it.
	if _, err := Parse(b); err != nil {
		return err
	}
	if err := atomicWrite(filepath.Join(Root(home), slug, FileYAML), b, 0o644); err != nil {
		return fmt.Errorf("projectdef: set secret %s: %w", name, err)
	}
	return nil
}

// RepoName is the worktree directory name: Name, else the basename of the
// path or remote without a trailing .git.
func (r Repo) RepoName() string {
	if r.Name != "" {
		return r.Name
	}
	src := r.Path
	if src == "" {
		src = r.Remote
	}
	src = strings.TrimRight(src, "/")
	if i := strings.LastIndexAny(src, "/:"); i >= 0 {
		src = src[i+1:]
	}
	return strings.TrimSuffix(src, ".git")
}

// ExpandPath resolves a Path repo against home (~ expansion).
func ExpandPath(home, p string) string {
	if p == "~" {
		return home
	}
	if strings.HasPrefix(p, "~/") {
		return filepath.Join(home, p[2:])
	}
	return p
}

func Load(home, slug string) (Project, error) {
	if err := ValidSlug(slug); err != nil {
		return Project{}, err
	}
	dir := filepath.Join(Root(home), slug)
	b, err := os.ReadFile(filepath.Join(dir, FileYAML))
	if err != nil {
		return Project{}, fmt.Errorf("projectdef: load %s: %w", slug, err)
	}
	d, err := Parse(b)
	if err != nil {
		return Project{}, fmt.Errorf("projectdef: load %s: %w", slug, err)
	}
	return Project{Slug: slug, Dir: dir, Def: d}, nil
}

// List returns every valid definition sorted by slug. A broken one is
// skipped but reported, so one bad yaml never hides the others.
func List(home string) ([]Project, error) {
	ents, err := os.ReadDir(Root(home))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("projectdef: list: %w", err)
	}
	var out []Project
	var errs []error
	for _, e := range ents {
		if !e.IsDir() || ValidSlug(e.Name()) != nil {
			continue
		}
		if _, err := os.Stat(filepath.Join(Root(home), e.Name(), FileYAML)); err != nil {
			continue
		}
		p, err := Load(home, e.Name())
		if err != nil {
			errs = append(errs, err)
			continue
		}
		out = append(out, p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Slug < out[j].Slug })
	return out, errors.Join(errs...)
}

var skeletonYAML = `# bough project definition. Lives outside every repo; never committed.
repos:
  - path: ` + Placeholder + `   # or remote: git@github.com:you/example.git
    branch: main
checks:
  fast: ""
  full: ""
# caches: [/root/.cache/go-build]
# env: {GOFLAGS: -mod=mod}
`

const skeletonSetup = `#!/bin/sh
# Runs once on the base image; the result is snapshotted as the orb image.
# "# bough:step <name>" starts a cached layer; "# bough:uses <file>" in a
# step copies that repo file to /bough-setup/lock/<repo>/<file> first.
set -e
# bough:step apt
apt-get update && apt-get install -y --no-install-recommends git ca-certificates
`

func Create(home, slug string) (Project, error) {
	if err := ValidSlug(slug); err != nil {
		return Project{}, err
	}
	dir := filepath.Join(Root(home), slug)
	if err := os.MkdirAll(Root(home), 0o755); err != nil {
		return Project{}, fmt.Errorf("projectdef: create %s: %w", slug, err)
	}
	// Mkdir (not MkdirAll) makes "exists" an error atomically.
	if err := os.Mkdir(dir, 0o755); err != nil {
		return Project{}, fmt.Errorf("projectdef: create %s: %w", slug, err)
	}
	if err := atomicWrite(filepath.Join(dir, FileYAML), []byte(skeletonYAML), 0o644); err != nil {
		return Project{}, fmt.Errorf("projectdef: create %s: %w", slug, err)
	}
	if err := atomicWrite(filepath.Join(dir, FileSetup), []byte(skeletonSetup), 0o755); err != nil {
		return Project{}, fmt.Errorf("projectdef: create %s: %w", slug, err)
	}
	return Load(home, slug)
}

func ReadFile(home, slug, name string) (string, error) {
	if err := checkName(slug, name); err != nil {
		return "", err
	}
	b, err := os.ReadFile(filepath.Join(Root(home), slug, name))
	if errors.Is(err, os.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("projectdef: read %s/%s: %w", slug, name, err)
	}
	return string(b), nil
}

func WriteFile(home, slug, name, text string) error {
	if err := checkName(slug, name); err != nil {
		return err
	}
	dir := filepath.Join(Root(home), slug)
	if _, err := os.Stat(dir); err != nil {
		return fmt.Errorf("projectdef: write %s/%s: %w", slug, name, err)
	}
	path := filepath.Join(dir, name)
	if name == FileYAML {
		d, err := Parse([]byte(text))
		if err != nil {
			return err
		}
		if err := CheckHost(home, d); err != nil {
			return err
		}
	} else if text == "" {
		if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("projectdef: delete %s/%s: %w", slug, name, err)
		}
		return nil
	} else if name == FileSetup {
		if err := checkSetup(home, slug, text); err != nil {
			return err
		}
	}
	mode := os.FileMode(0o644)
	if strings.HasSuffix(name, ".sh") {
		mode = 0o755
	}
	if err := atomicWrite(path, []byte(text), mode); err != nil {
		return fmt.Errorf("projectdef: write %s/%s: %w", slug, name, err)
	}
	return nil
}

// checkSetup rejects bad step markers and, when project.yml loads, a
// `bough:uses` file no repo tracks.
func checkSetup(home, slug, text string) error {
	steps, err := ParseSteps(text)
	if err != nil {
		return fmt.Errorf("projectdef: %w", err)
	}
	p, err := Load(home, slug)
	if err != nil {
		return nil
	}
	for _, s := range steps {
		if _, err := StepLockfiles(home, p, s.Uses); err != nil {
			return fmt.Errorf("projectdef: %w", err)
		}
	}
	return nil
}

func checkName(slug, name string) error {
	if err := ValidSlug(slug); err != nil {
		return err
	}
	if !slices.Contains(EditableFiles, name) {
		return fmt.Errorf("projectdef: %q is not an editable file (want one of %s)", name, strings.Join(EditableFiles, ", "))
	}
	return nil
}

// UsesDockerfile reports whether the build uses the Dockerfile; it wins
// over setup.sh when both exist.
func (p Project) UsesDockerfile() bool {
	_, err := os.Stat(filepath.Join(p.Dir, FileDockerfile))
	return err == nil
}

func atomicWrite(path string, b []byte, mode os.FileMode) error {
	f, err := os.CreateTemp(filepath.Dir(path), "."+filepath.Base(path)+".tmp-*")
	if err != nil {
		return err
	}
	tmp := f.Name()
	defer os.Remove(tmp) // no-op after a successful rename
	if _, err := f.Write(b); err != nil {
		f.Close()
		return err
	}
	if err := f.Chmod(mode); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
