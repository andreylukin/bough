# Orbs: local and project sessions — implementation contract

Status: contract for the orbs-wf1 run. Six areas build against this file in
parallel. Where this file names an identifier, use it verbatim; where it is
silent, decide inside your own area and do not change another area's files.
LSP broker and cloud runtimes are out of scope.

## 0. The model in one screen

- Every session has exactly one **mode**, fixed at creation and written into
  the first history `meta` entry: `"mode": "local"` or
  `"mode": "project", "project": "<slug>"`. A meta entry with no `mode`
  (every session recorded before this run) reads as `local`.
- **local** (default): process runs in `~` on the host, shell unrestricted,
  `tools.write` / `tools.patch` NOT registered, a read-only prompt section,
  scratchpad writable, no `checkpoints` service.
- **project**: the child process still runs on the host. A plugin row `orb`
  creates git worktrees under `~/.bough/orbs/<session>/<repo>`, starts a
  Linux container from the project's snapshot image, and provides an
  exec seam; `tools.bash` and background jobs run through it. Worktrees
  and the scratchpad are bind-mounted **at their identical host paths**
  inside the container, so there is no path translation anywhere:
  `tools.write`/`patch`/`view` act on host paths, and the same path is
  valid inside the container. Checkpoints work unchanged against the
  host worktree (the child `chdir`s into the primary worktree).
- The project definition lives in `~/.bough/projects/<slug>/`, never in a
  repo, and IS the project: serve derives its list from that directory
  and keys everything by the slug.
  The display name is `name:` in `project.yml`; renaming a project
  rewrites that one line and NEVER renames the directory.
- `MEMORY.md` in that directory is the project's standing brief. It is a
  file like `AGENTS.md`, not a memory system: `context-md` prepends it to
  every session in the project, deduped by section, and the only things
  that write it are the person (the editor on the project page) and an
  agent asked to remember something (the ordinary write tool; the
  injected header names the path). No hook, no per-turn extraction, no
  summarisation — nothing automatic ever writes it. It is not a build
  input either (`ImageHash` skips it), so editing the brief never
  rebuilds an image or recreates a container.
- Every project has a **main thread**: one long-lived session in the
  project's orb, created the first time the project is opened or
  messaged, recorded in `meta.json` as `mains: {slug: id}` (serve state,
  not part of the definition — a project directory copied to another
  machine must not claim a foreign session). Messaging the project is
  messaging main. Every project session started from the web is a CHILD
  of main, which is what makes a thread's finish or failure land
  somewhere a person reads: `Supervisor.report` -> `notifyFrom(parent)`
  returns early for a parentless session.
  Orbs are per SESSION, so a project with a main and N threads runs N+1
  containers; `Supervisor.EndProject` stops all of them and is what both
  archive and delete go through.
  **Not parented**, this run: a session started from the CLI or the TUI
  with `bough --project <slug>`, and every project session that existed
  before the migration to slug keying. They are listed on the project
  page (the threads list is the union of main's children and everything
  filed under the slug), but they report to nobody.

Why the child sets up its own orb (not serve): the session id is only
known once the child's history row mounts (supervisor learns it after
spawn), and a TUI `bough --project x` must work with no serve at all.

## 1. Packages and Go identifiers

### 1a. `go/internal/container` — runtime (area: runtime)

```go
package container

// Runtime is one container engine. Implementations shell out to a CLI;
// no cgo, no daemon SDKs.
type Runtime interface {
	Name() string // "apple", "nerdctl", "podman", "fake"
	// Available reports nil when the engine is installed AND usable
	// (Apple: binary present and `container system status` running).
	// The error says what to run to fix it.
	Available(ctx context.Context) error
	// Build builds an image from a Dockerfile in dir, tagging it tag.
	// Output streams to log line by line as it arrives.
	Build(ctx context.Context, spec BuildSpec, log io.Writer) error
	// Commit runs spec.Script on spec.Base in a throwaway container with
	// spec.Mounts, then snapshots the result as tag (setup-script path).
	Commit(ctx context.Context, spec CommitSpec, log io.Writer) error
	ImageExists(ctx context.Context, tag string) (bool, error)
	// Start creates and starts a long-lived container (`sleep infinity`
	// entrypoint) named spec.Name; an existing stopped one of that name
	// is started instead of recreated.
	Start(ctx context.Context, spec RunSpec) error
	// Command returns an unstarted *exec.Cmd that runs argv inside the
	// named container. The caller owns Stdout/Stderr/process group/
	// timeout exactly as for a host command.
	Command(ctx context.Context, name string, opt ExecOptions, argv ...string) *exec.Cmd
	Stop(ctx context.Context, name string) error
	Remove(ctx context.Context, name string) error // stopped or not; missing is nil
	Inspect(ctx context.Context, name string) (State, error)
	CreateVolume(ctx context.Context, name string) error // exists is nil
}

type BuildSpec struct {
	Dir        string // build context
	Dockerfile string // path, usually Dir/Dockerfile
	Tag        string
}

// CommitSpec is a setup-script build with one COPY+RUN layer per step
// (docs/orb-speed.md §2). No bind mounts: Apple 1.1.0 has no commit verb,
// so Commit is a generated Dockerfile build, and a build cannot see mounts.
type CommitSpec struct {
	Base      string // generic base image, DefaultBase unless project.yml says otherwise
	Steps     []Step
	FilesRoot string // every Step.Files path lies under it; lands at /bough-setup/lock/<rel>
	Tag       string
}

type Step struct {
	Name   string   // script is steps/<nn>-<name>.sh
	Script []byte   // preamble + step body, run with ScriptArgv
	Files  []string // the step's `bough:uses` files, COPYed right before its RUN
}

type Mount struct {
	Source   string // host path, or volume name when Volume
	Target   string // guest path
	Volume   bool
	ReadOnly bool
}

type RunSpec struct {
	Name   string // OrbName(session)
	Image  string
	Mounts []Mount
	Env    []string
	Workdir string
	CPUs   int    // 0 = engine default
	Memory string // "" = engine default, e.g. "8G"
}

type ExecOptions struct {
	Workdir     string
	Env         []string
	Interactive bool // attach stdin (-i)
}

type State string

const (
	StateMissing State = "missing"
	StateRunning State = "running"
	StateStopped State = "stopped"
)

const DefaultBase = "docker.io/library/debian:bookworm"

// ErrNotImplemented is what the nerdctl stub returns from every method.
var ErrNotImplemented = errors.New("container: runtime not implemented yet")

// Default picks the runtime for this OS: Apple on darwin; on linux the
// first of nerdctl/podman on PATH (stub); otherwise Unsupported.
func Default() Runtime

// OrbName is the container name for a session: "bough-orb-" + session id.
func OrbName(session string) string
```

Files: `runtime.go` (interface, types, Default, OrbName), `apple.go`
(`type Apple struct{ Bin string }`, `NewApple() *Apple`, Bin defaults to
`/opt/homebrew/bin/container` then `$PATH`), `linux.go`
(`type Nerdctl struct{}`, `type Podman struct{}` — every method returns
`fmt.Errorf("container: %s: %w", name, ErrNotImplemented)`; `Command`
returns a cmd whose `Err` field is set), `fake.go` (`type Fake struct`,
`NewFake() *Fake` — in-memory images/containers/volumes; `Command` runs
argv on the HOST with `Workdir` as dir so exec tests are real; records
calls in `Fake.Calls []string` for assertions; `Fake.FailBuild error`
knob). Fake is in the non-test package so other areas' tests import it.

Apple CLI mapping (1.1.0, flags checked against `--help` on this machine):
`container build -t TAG -f FILE --progress plain DIR`;
`container run -d --init --name N -v SRC:DST[:ro] --mount type=volume,source=V,target=DST -w DIR -e K=V -c CPUS -m MEM IMAGE sleep infinity`;
`container start N` (existing stopped container);
`container exec [-i] [-w DIR] [-e K=V] N ARGV...`; `container stop N`;
`container delete --force N` (`rm -f` alias); `container inspect N` (no
format flag; parse its default JSON output, empty/err => missing).
UNVERIFIED because the subcommands are plugins that only print help once
`container system start` has run — the implementer confirms with the live
test and records the real shape in apple.go: `container image list --format json`
(fallback: `container image inspect TAG` exit status), `container volume create N`,
the `--mount type=volume` syntax (fallback `-v NAME:DST`), and that
`--mount`/`-v` of a named volume works. There is NO commit verb in 1.1.0:
Commit writes a temp context dir (setup.sh + Files) and a Dockerfile
(`FROM base`, then per step `COPY steps/<nn>-<name>.sh`, `COPY lock/<repo>/<file>` for
its declared files, `RUN <interpreter> /bough-setup/steps/<nn>-<name>.sh`; no `ENV`; the
interpreter taken from the script's `#!` line, else `sh`; resume.sh runs the same way)
and calls Build; documented in apple.go.
Exec env: `container exec` does NOT inherit the host environment, so the
caller must pass every variable the command needs via `ExecOptions.Env`
(at minimum `BOUGH_SCRATCH`, `HOME`, `TERM=dumb`, and project `env`).
Kill semantics: whether SIGKILL of the `container exec` client ends the
guest process is unverified; the live test asserts it (start `sleep 300`,
kill the client, `exec ps` shows no sleep). If it does not, `Command`'s
caller must also run `container exec N pkill -g`-style cleanup — Apple's
Cancel func is the place, owned by runtime via an exported
`KillFunc(name string, cmd *exec.Cmd) func() error`.

### 1b. `go/internal/projectdef` — project definition (area: projectdef)

```go
package projectdef

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
	Repos   []Repo            `yaml:"repos"`
	Checks  Checks            `yaml:"checks,omitempty"`
	LSP     []string          `yaml:"lsp,omitempty"`     // roots; parsed, unused this run
	Base    string            `yaml:"base,omitempty"`    // setup-script base; "" = container.DefaultBase
	Caches  []string          `yaml:"caches,omitempty"`  // guest dirs bound to ~/.bough/cache/<slug>/<sha>, e.g. /root/.cache/go-build
	Env     map[string]string `yaml:"env,omitempty"`
	Secrets map[string]string `yaml:"secrets,omitempty"` // env name -> keychain:<service>; refs only
	Redact  *bool             `yaml:"redact,omitempty"`  // nil = on; false opts out of secret redaction
	CPUs    int               `yaml:"cpus,omitempty"`
	Memory  string            `yaml:"memory,omitempty"`
}

// Project is one definition on disk.
type Project struct {
	Slug string // directory name: [a-z0-9][a-z0-9-]{0,62}
	Dir  string // ~/.bough/projects/<slug>
	Def  Def
}

// DisplayName is Def.Name, else the slug. Def.Name is `name:` in
// project.yml and is the ONLY place a project's display name lives.
func (p Project) DisplayName() string

const (
	FileYAML       = "project.yml"
	FileDockerfile = "Dockerfile"
	FileSetup      = "setup.sh"
	FileResume     = "resume.sh"
	FileMemory     = "MEMORY.md"
)

// EditableFiles is the order the orb panel's tabs and `bough project
// show` use; the project page leads with MEMORY.md instead.
var EditableFiles = []string{FileYAML, FileDockerfile, FileSetup, FileResume, FileMemory}

// Entry is one directory under Root, parsed or not.
type Entry struct {
	Slug string
	Dir  string
	Def  Def
	Err  error // why project.yml did not parse; nil when it did
}

func Root(home string) string                     // home/.bough/projects
func ValidSlug(s string) error
func List(home string) ([]Project, error)         // dirs with a project.yml; bad yaml => entry skipped + returned in a joined error
// ListAll keeps the broken ones, with the error on the Entry: a
// definition that does not parse still has a page, and the editor that
// fixes it is on that page. Sorted by slug.
func ListAll(home string) []Entry
func Load(home, slug string) (Project, error)
func Create(home, slug string) (Project, error)   // writes skeleton project.yml + setup.sh; exists => error
// CreateEmpty writes project.yml with `repos: []`, an optional `name:`
// and NOTHING else: the shape a project that was only ever a label
// migrates into, and what `POST /api/projects` makes. No skeleton (its
// placeholder repo is refused on the next write) and no setup.sh
// (which would be built and snapshotted).
func CreateEmpty(home, slug, name string) (Project, error)
// SetName replaces or inserts the top-level `name:` line TEXTUALLY:
// project.yml ships comments and is hand-edited, and marshalling Def
// over it would drop every comment and reorder every key. The directory
// is never renamed — orbs, images, caches and every past session are
// keyed by slug on disk.
func SetName(home, slug, name string) error
func ReadFile(home, slug, name string) (string, error)   // missing => "", nil
// WriteFile validates: name in EditableFiles; project.yml must parse
// (zero repos is fine: a project need not have a checkout); writing
// Dockerfile when setup.sh exists (or vice versa) is allowed and
// Dockerfile WINS at build time. Empty text deletes the file, EXCEPT
// MEMORY.md, which is plain text and whose empty body is an empty file
// (project.yml cannot be deleted). Atomic temp+rename.
func WriteFile(home, slug, name, text string) error
func Parse(b []byte) (Def, error)

// ImageHash is sha256 over build inputs only (docs/orb-speed.md §1): Base
// (or BaseTag); on the setup.sh path its bytes plus each step's
// `# bough:uses` files at the repo's base ref (read from the host source
// checkout / cached clone, never a worktree); on the Dockerfile path every
// project dir file except project.yml, resume.sh and MEMORY.md. Hex,
// first 12 chars.
func ImageHash(home string, p Project) (string, error)
// ParseSteps splits setup.sh on `# bough:step <name>`; the preamble before
// the first marker is prepended to every step; no markers = one step.
func ParseSteps(text string) ([]Step, error)
// StepLockfiles resolves `bough:uses` files; a file no repo tracks is an error.
func StepLockfiles(home string, p Project, uses []string) ([]Lockfile, error)
func ImageTag(slug, hash string) string // "bough-orb/<slug>:<hash>"
```

`Parse` validates `secrets`: names match `^[A-Za-z_][A-Za-z0-9_]*$`, refs
are `keychain:<service>` (non-empty, at most 200 bytes, no whitespace or
control characters), and a name can't be in both `env` and `secrets`.
`ImageHash` does not read project.yml beyond `base`, so secrets, env,
checks, identity, cpus, memory and caches never rebuild the image;
secrets never reach setup.sh or the Dockerfile.
`SetSecret(home, slug, name, ref)` sets one and drops a same-named env.
See docs/secrets.md.

Remote repos are cloned once, bare, into `~/.bough/orbs/cache/<slug>/<name>.git`
(fetched on each orb start); worktrees are added from there. Path repos add
worktrees from the user's own checkout.

### 1c. `go/internal/orb` — lifecycle (area: projectdef)

```go
package orb

type Status string

const (
	StatusNone     Status = ""         // local session
	StatusBuilding Status = "building" // image build in progress
	StatusStarting Status = "starting" // worktrees/container/resume
	StatusRunning  Status = "running"
	StatusStopped  Status = "stopped"
	StatusFailed   Status = "failed"
)

// State is ~/.bough/orbs/<session>/state.json, written by the child
// (atomic temp+rename on every transition) and read by serve.
type State struct {
	Session   string            `json:"session"`
	Project   string            `json:"project"` // slug
	Status    Status            `json:"status"`
	Image     string            `json:"image,omitempty"`     // tag
	Container string            `json:"container,omitempty"` // container.OrbName
	Worktrees map[string]string `json:"worktrees,omitempty"` // repo name -> host path
	Primary   string            `json:"primary,omitempty"`   // host path of repos[0] worktree
	Error     string            `json:"error,omitempty"`
	PID       int               `json:"pid,omitempty"`       // child owning the orb; serve shows "stopped" when dead
	UpdatedAt time.Time         `json:"updatedAt"`
}

type Orb struct { /* rt, home, session, project, state */ }

func Dir(home, session string) string          // home/.bough/orbs/<session>
func ReadState(home, session string) (State, error) // missing => State{}, nil
func ImageLogPath(home, slug string) string    // home/.bough/orbs/images/<slug>/build.log

// EnsureImage builds ImageTag(slug, hash) when missing. Serialized per
// slug across processes (serve's Build button and any number of
// children) with a flock on images/<slug>/build.lock (unix; no-op lock
// on windows). It re-checks ImageExists AFTER taking the lock and
// re-hashes (re-loading the definition) and returns without touching
// build.log/build.json when a waiter finds that image already built; a
// waiter with a log tails the running build.log into it; only the process that actually builds truncates
// and writes build.log, and after success prunes bough-orb/<slug>:* tags
// no container uses; also tees to
// log when non-nil. Writes images/<slug>/build.json:
// {"tag","hash","state":"building|ok|failed","startedAt","endedAt","error"}.
func EnsureImage(ctx context.Context, rt container.Runtime, home string, p projectdef.Project, log io.Writer) (tag string, err error)
func ReadBuild(home, slug string) (Build, error)
type Build struct {
	Tag       string    `json:"tag"`
	Hash      string    `json:"hash"`
	State     string    `json:"state"` // "", "building", "ok", "failed"
	StartedAt time.Time `json:"startedAt"`
	EndedAt   time.Time `json:"endedAt,omitempty"`
	Error     string    `json:"error,omitempty"`
}

// Open prepares a session's orb: EnsureImage, worktrees (branch
// "bough/<session>" off Repo.Branch; existing worktree reused on resume),
// cache dirs ~/.bough/cache/<slug>/<sha256(guest path)[:10]> (host binds), Start with mounts
// {each worktree, scratchDir, the project definition dir read-WRITE so the
// agent can edit MEMORY.md} at identical paths — plus, for Path repos,
// the source checkout's .git dir at its identical path, because a
// worktree's .git FILE points at <source>/.git/worktrees/<name> and git
// inside the container fails without it (remote repos: the cache .git
// dir likewise) — then resume.sh (if any)
// via Exec with output appended to Dir/resume.log between start/end
// lines carrying timestamps and duration. A resume.sh failure
// marks Failed but returns the Orb usable (the agent can fix it).
func Open(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratchDir string) (*Orb, error)
func (o *Orb) State() State
// Command is the exec seam: argv run inside the container, workdir =
// Primary, env = execEnv (exec does not inherit the host env). If the container is not running (stopped from
// serve, engine restarted) Command first re-Starts it and re-runs
// resume.sh, so a stop never strands a live child.
func (o *Orb) Command(ctx context.Context, argv ...string) *exec.Cmd
// Root is Dir(home, session): the directory holding every worktree.
func (o *Orb) Root() string
func (o *Orb) Stop(ctx context.Context) error   // container stop; state Stopped; worktrees kept
func Remove(ctx context.Context, rt container.Runtime, home, session string) error // rm container, git worktree remove, rm Dir
```

`execEnv` builds every exec's env (Command and resume.sh) in this order,
later names winning: coreEnv (`HOME`, `TERM`, `BOUGH_SCRATCH`);
identityEnv (`GH_TOKEN`, `GIT_CONFIG_*`, host prefixes); proxy env,
`BOUGH_HOST`, `PATH`; `Def.Env`; resolved secrets sorted by name. Refs
are re-read from `projectdef.Load` on every exec (falling back to the
Def captured at Open), so a secret added mid-session applies to the next
command. An unresolved ref is left out, with one stderr line per name per
orb. Secrets never go into the run env, which inspect shows.

Redaction (`internal/orb/redact.go`): resolved values of 8+ bytes become
`[redacted:NAME]` in tools.bash output (foreground result, error text and
background job output, streamed with a held-back tail so a value split
across chunks is still caught), in every history entry (the orb row sets
`Store.SetRedact`), and in resume.log. `redact: false` in project.yml turns
it off. The command text the model wrote is recorded as written.

### 1d. Identity, tools and egress

A project session acts as the user, with the same permissions as their
own shell; it only isolates file changes.

- **Tools**: `projectdef.BaseDockerfile` (embedded `base.Dockerfile`) is
  built as `projectdef.BaseTag()` = `bough-orb/base:<sha12>` by
  `orb.EnsureBase` before any setup-script project whose `base` is empty.
  It carries gh, aws, kubectl, helm, helmfile, sops, just, gcx, argocd,
  uv and python3. `ImageHash` hashes the base tag, so editing the base
  rebuilds every project on it.
- **Host identity (opt-in)**: nothing from the host is lent by default.
  A project lists what it needs in `identity:` (`bough project
  add-identity <slug> gh|.aws|.kube:rw`). `gh` passes the host's
  `gh auth token` as `GH_TOKEN` (cached 5 min, per exec, never in run env)
  and sets `credential.https://github.com.helper=!gh auth git-credential`.
  A `<dir>` entry bind-mounts `~/<dir>` read-only at `/root/<dir>`
  (`<dir>:rw` for read-write). `.ssh`, `.gnupg`, `.bough`, `Library`,
  `.local`, `.docker`, `.config` itself and its shell/git/gh config are
  refused, and the list is not part of the image hash. The host's
  user.name/email reach git through `GIT_CONFIG_*` env (the host gitconfig
  is not mounted); host `AWS_PROFILE`/`AWS_REGION`/`AWS_DEFAULT_REGION`
  pass through.
- **Secrets**: project.yml `secrets:` maps env names to
  `keychain:<service>` refs (stored under account `bough`, read from any
  account). `internal/secrets` resolves them on the host (5 min cache,
  failures cached 30s) and every exec passes them as
  `ExecOptions.Secrets`: argv carries `-e NAME` only, the value reaches
  the `container exec` client through its own environment. Names that
  every exec sets are rejected. Commands see the values, so output that
  prints one (`env`, `echo $NAME`) lands in the tool result and history;
  nothing scrubs it. Asked secrets are stored as `bough/<slug>/<NAME>`.
  resume.log is on the host and is not scrubbed.
- **Egress**: per-app VPNs (for example zero-trust access clients) do not tunnel the
  VM bridge, so the child runs an HTTP/CONNECT proxy on the guest's
  gateway IP (its resolv.conf nameserver) and sets `HTTPS_PROXY` and
  friends per exec. Stop closes it; a restart reopens it.
- **bough / MCP**: the host bough is a macOS binary and MCP grants live in
  the keychain, so the guest's `bough` is a shim at
  `$BOUGH_SCRATCH/.bin/bough` (first on the exec `PATH`) that POSTs its
  args to `$BOUGH_HOST/bough/exec` on the proxy; the host runs its own
  bough (only the `mcp` and `project` subcommands) and returns stdout, stderr and exit.

## 2. Session mode (area: session-mode)

### Choosing the mode

- CLI: `bough --project <slug>` => project; `bough --local` (explicit,
  optional) => local; neither => local. Both => exit 2. Works with
  `--headless`, TUI and `-p`.
- Env (serve → children, and a person's shell): `BOUGH_MODE=local|project`,
  `BOUGH_PROJECT=<slug>`. Flags win over env. main reads then
  `os.Unsetenv`s both (same reason as BOUGH_ORIGIN: the agent's own bough
  runs must not inherit it).
- `BOUGH_PROJECT_DIR=<host project dir>` is separate and never chooses a
  mode: serve sets it on a LOCAL session filed under a project, so
  `context-md` prepends that project's `MEMORY.md` and `tools.write` may
  edit it (service `session-project-dir`, local mode only; a project
  session gets the same directory from its slug). It is derived from
  `SessionMeta.Project` at EVERY start, not from spawn args, so it
  survives a serve restart — a session already running does not pick up a
  new assignment until its next start. Read and unset like the others.
- `BOUGH_PROJECT_MAIN=1` marks the one session that is a project's MAIN
  THREAD: the session the project page talks to and the parent of every
  other session in the project. serve derives it at EVERY start from the
  main it recorded, like `BOUGH_PROJECT_DIR`, so a restart does not lose
  it (service `session-main`; `plugins/orb` turns it into the "you are
  the main thread / you are a thread of ..." line in the prompt, pairing
  it with `BOUGH_SPAWNED_BY` for the other side). Read and unset like the
  others.
- Resume (`-c`, `-r id`, history.file): the mode comes from the resumed
  file's meta, never flags/env. A `--project` that disagrees with the file
  prints a notice and is ignored.
- A project slug that does not `projectdef.Load` => exit 2 with the error
  (headless) / fatal (tui), before any row mounts.

### Kernel service keys (provided by main before mount)

| key | type | provider | consumers |
|---|---|---|---|
| `session-mode` | `string` — `"local"` or `"project"` | launcher (cmd/bough/main.go) | history, tools, orb, loop prompt (via tools' section) |
| `session-project` | `string` — slug, `""` for local | launcher | history, orb |
| `session-main` | `bool` — this session is its project's main thread | launcher | orb (prompt role) |
| `orb` | `OrbExec` (below) | plugins/orb | tools (optional; absent in local) |
| `orb-state` | `interface{ State() orb.State }` | plugins/orb | ui/session (optional) |

On resume main reads the file's meta first (history.Read, first entry)
and provides the values from it. Consumers MUST `kernel.Get` these at use
time with a default of local, so an old config tree or a test context
without them behaves as local.

`OrbExec` is declared structurally in the consumer (no plugin imports):

```go
// in plugins/tools
type orbExec interface {
	// Command runs argv in the container; nil error-free *exec.Cmd.
	Command(ctx context.Context, argv ...string) *exec.Cmd
	// Root is ~/.bough/orbs/<session>: write/patch refuse paths outside
	// Root and $BOUGH_SCRATCH in project mode.
	Root() string
}
```

### History (`plugins/history`)

- `meta` data gains `mode` (always written for new files) and `project`
  (project only). For project sessions `cwd` records `~` as started; the
  orb row appends nothing to meta (meta is immutable once written).
- `SessionInfo` gains `Mode string` and `Project string`, filled by `List`
  from meta (`""` mode normalized to `"local"`).
- `checkpoints` is provided ONLY when mode is project. Local sessions get
  none (loop already treats it as optional). Checkpoints keep using
  `os.Getwd()`, which the orb row sets to the primary worktree; repos
  other than the primary are not checkpointed this run (documented gap).

### The orb row (`plugins/orb`, new; plugin name `orb`, row id `orb`)

- `Inject() = []string{"history", "scratch"}` — Inject lists SERVICE
  KEYS (kernel/loader.go), not row ids; the scratchpad row's plugin is
  `scratchpad` but its service key is `scratch`. In local mode Apply
  returns nil and provides nothing.
- Mount order is decided by Inject keys, not bough.yml position, and
  `tools-basic` injects only `codemode`, so tools may mount BEFORE orb.
  Therefore tools must never read `orb` at Apply (see Tools below).
- Project: `orb.Open(ctx, rt, home, sessionID, project, scratchDir)`
  where sessionID = history path basename, scratchDir = `scratch` service
  `Dir()`. Then chdir to `state.Primary` through a package var
  `var chdir = os.Chdir` (tests swap it: os.Chdir is process-global and
  would race every other `t.Parallel()` test; the one test that needs a
  real chdir runs in the e2e subprocess), provide `orb` and `orb-state`,
  set prompt section `orb` (container, repos, checks.fast/full, "your
  shell runs in a Linux container; files under <root> are shared with the
  host"), plus the blocked-verification rule (docs/secrets.md §5) and,
  when `missingEnv` (plugins/orb/envcheck.go) finds env that resume.sh or
  the checks reference but nothing sets, `Unset env referenced by
  resume.sh/checks: A, B. ...`; the same list goes to stderr once per open
  as `bough: orb: <slug>: unset env A, B (resume.sh/checks)`. Effect on unmount: `Stop` (never Remove — resume reuses it).
- Open failure: row error names the row and wraps
  (`orb: open %s: %w`); state.json says failed so serve shows it.
- `runtime.Available` failing => the same error path with the fix
  ("run `bough update` or `container system start`").
- Config: `runtime: apple|nerdctl|podman|fake` (default: container.Default);
  `fake` exists for e2e/tests.
- bough.yml gets the row after `scratchpad` with a WHY comment.

### Tools (`plugins/tools`)

- Local: do not `RegisterTool("write"/"patch")`, do not Describe them.
  `view` stays. The prompt section `mode` (`internal/orb.LocalPromptSection`,
  ~4 lines: read-only on local files; shell for remote systems; throwaway
  files only under $BOUGH_SCRATCH; start a project session to change code)
  is set by the ORB row in local mode, not tools: tools mounts before the
  loop, so an Apply-time prompt-sections lookup reloads tools, which
  re-provides turn-stats and reloads loop and ui mid-startup (headless
  lost its input and hung). tools re-exports the const.
- Project: `bash` foreground and `jobs.start` resolve `orb` with
  `kernel.Get[orbExec]` AT CALL TIME and build the command via
  `orb.Command(ctx, "sh", script)` instead of `exec.CommandContext(ctx,
  "sh", script)`. If mode is project and `orb` is absent (row pending or
  failed), bash returns an error ("tools: bash: project orb not ready:
  see the orb row") — it must NEVER fall back to running on the host.
  The script temp file is created in `$BOUGH_SCRATCH` (mounted at the
  same path) instead of os.TempDir. Process-group kill, timeout,
  WaitDelay stay as is (see the kill-semantics note in §1a).
  write/patch refuse paths outside `orb.Root()`, `$BOUGH_SCRATCH` and
  `~/.bough/projects/<slug>` (the agent may edit its own project; any
  session, local included, changes definitions through the validated
  `bough project` CLI, which the guest's shim relays to the host
  definition) with an error naming the allowed roots; same "not ready"
  error when `orb` is absent.
- Mode (`session-mode`, provided by main before any row mounts) is read
  once at Apply to decide registration; the `orb` service is not.
- `tools.secret(name, reason, project?)` (plugins/ask) asks the user for a
  credential with a secret ask, stores it with `secrets.Store` under
  `bough/<slug>/<NAME>` and adds the ref with `projectdef.SetSecret`. It
  returns only the ref; declining throws `secret: user declined`.
- History: a secret ask logs `ask/answer` as `{text:"[secret stored]",
  secret:true}`, so export, wiki, serve lines and the TUI never see the
  value; the TUI masks the composer and headless never echoes the line.
  This covers the answer only: a command that prints the env is not
  redacted.

## 3. Serve JSON API (area: serve-api)

All existing endpoints keep their shapes; new fields are optional.

### Types (Go, `internal/serve`)

```go
type Project struct { // existing, extended
	ID   string `json:"id"`
	Name string `json:"name"`
	Slug string `json:"slug,omitempty"` // projectdef slug when an orb definition is attached
}

type OrbSummary struct {
	Slug   string `json:"slug"`
	Image  string `json:"image"`            // tag for the CURRENT hash
	Built  bool   `json:"built"`            // that tag exists in the runtime
	Build  string `json:"build,omitempty"`  // orb.Build.State
	Error  string `json:"error,omitempty"`  // definition parse error
}
```

The DIRECTORY is the project: `serve.Project{Slug, Name, Error}` is
derived from `projectdef.ListAll(home)` on every read, so a definition the
agent wrote with the file tools is a project without serve being told, and
one it removed stops being one. `meta.json` holds no project table (it is
`"version": 2`); `SessionMeta.Project` holds a SLUG. A version-1 file
migrates on boot: the label's name is written into `project.yml` first
(`projectdef.SetName`), a label with no definition becomes one with no
repos, and every `SessionMeta.Project` is rewritten from id to slug.
Running an OLD binary against a version-2 file is not supported.

Deleting a project removes `~/.bough/projects/<slug>` AND the state a
project of the same name would inherit — `~/.bough/orbs/images/<slug>`,
`~/.bough/orbs/cache/<slug>`, `~/.bough/cache/<slug>` — and unassigns its
sessions. It never deletes a conversation.

### Endpoints

| method + path | request | response |
|---|---|---|
| `GET /api/projects` | — | `{"projects": [Project & {"orb": OrbSummary}]}`; every project has one |
| `POST /api/projects` | `{"name": "..."}` | `{"project": Project}`; creates `~/.bough/projects/<slugify(name)>` with no repos. 400 a name with no letter or digit, 409 the slug is taken |
| `GET /api/projects/{slug}` | — | `ProjectDetail`: the project, `main` (the main thread's id, `""` until it has one — reading the page never creates one), `mainOrb` (main's `OrbState`), `orbs` (every session orb for the slug), and `threads`, the union of main's children and every session filed under the slug, oldest first |
| `POST /api/projects/{slug}/rename` | `{"name": "..."}` | `{"ok": true}`; writes `name:` into project.yml textually. The directory keeps its slug |
| `POST /api/projects/{slug}/message` | `{"text": "..."}` | `{"ok": true, "main": "<id>"}`; creates the main thread on the first message and sends to it. 409 with the question when main has a pending ask |
| `POST /api/projects/{slug}/archive` | — | `{"ok": true, "archived": N}`; stops main, every thread and each of their containers, then archives those conversations. Nothing on disk is deleted and a message starts the project again |
| `DELETE /api/projects/{slug}` | — | `{"ok": true}`; see above — the web confirms by making you type the slug |
| `GET /api/projects/{slug}/orb` | — | `OrbDetail` below; 404 unknown slug, 400 when the path is a pre-slug label id |
| `PUT /api/projects/{slug}/orb/files/{name}` | `{"text": "..."}` | `{"ok": true, "orb": OrbSummary}`; name ∈ project.yml, Dockerfile, setup.sh, resume.sh, MEMORY.md; 400 with parse error |
| `POST /api/projects/{slug}/orb/build` | `{}` | 202 `{"build": Build}`; runs orb.EnsureImage in a goroutine; 409 while building |
| `GET /api/projects/{slug}/orb/build/log?offset=N` | — | `{"text": "...", "offset": M, "state": "building|ok|failed|"}` — bytes from N of build.log, max 256 KiB per call; client polls every 1 s while building |
| `POST /api/sessions/{id}/project` | `{"project": "<slug>"}` | `{"ok": true}`; `""` unassigns. 409 for a session whose history says `mode: project` — its project is recorded there and nothing re-files it |
| `GET /api/sessions/{id}/orb` | — | `{"orb": OrbState}` (`orb.State` JSON, status forced to `stopped` when PID dead and status was running/starting; `up: true` when the container runs, including after a failed setup); local => `{"orb": null}` |
| `POST /api/sessions/{id}/orb/stop` | — | `{"ok": true}`; runtime Stop on OrbName. Allowed while the child is live: the child's `Orb.Command` re-starts a stopped container on the next exec. serve marks state.json `stopped` before the runtime Stop (restored if Stop fails); the child writes `running` again on restart. A background job that dies while the orb is marked stopped (or not running) records `stopped: true`, shows `[stopped with the orb]` and queues no wake notice. The web asks to confirm only when the session has running jobs |
| `POST /api/sessions` | `{"cwd", "prompt", "mode"?: "local"\|"project", "project"?: "<slug>"}` | 201 `{"session": Row, "queued": bool}`. mode omitted => local. project mode: the slug must name a project (400 otherwise), cwd defaults to home and is ignored for the child dir; child env gets `BOUGH_MODE=project BOUGH_PROJECT=<slug>`, and the session is started as a THREAD of the project's main thread (`spawnedBy` = main, created if the project has none), so it goes through the background-agent queue and reports its finished turns to main |

```go
type OrbDetail struct {
	Project Project           `json:"project"`
	Files   map[string]string `json:"files"`   // every EditableFiles name; "" when absent
	Hash    string            `json:"hash"`    // "" when definition invalid
	Summary OrbSummary        `json:"orb"`
	Build   orb.Build         `json:"build"`
	Orbs    []OrbState        `json:"orbs"`    // state.json of sessions with this slug, newest first
	Runtime struct {
		Name      string `json:"name"`
		Available bool   `json:"available"`
		Error     string `json:"error,omitempty"`
	} `json:"runtime"`
}
```

### Row additions (`GET /api/sessions`, `GET /api/sessions/{id}`)

`mode: "local"|"project"` (always), `orb?: {"project": slug, "status": orb.Status}`
read from state.json (cheap stat per project session; not for local).
The supervisor's resume path (`start` with an id) needs no mode env: the
child reads the file's meta.

`Supervisor.Create(cwd, prompt string)` becomes
`Create(opt CreateOptions) (string, error)` with
`type CreateOptions struct{ Cwd, Prompt, Mode, Slug string }`; update the
two existing callers inside internal/serve. `Options` gains
`Runtime container.Runtime` (nil => container.Default()) and `Home string`
("" => derived from HistDir's parent's parent) so tests inject Fake.
A project child is spawned with `cmd.Dir = home` and gets
`BOUGH_MODE`/`BOUGH_PROJECT` appended after `BOUGH_ORIGIN=web`, so a
serve process that itself inherited those vars cannot leak them into a
local child (strip any existing `BOUGH_MODE=`/`BOUGH_PROJECT=` from
`cmd.Env` for every child). PID liveness: "dead" = `kill(pid, 0)` fails
OR the pid is not one of the supervisor's own children and state.json is
older than the process start of serve; tolerate PID reuse by preferring
the supervisor's live-child table when it knows the session.

## 4. Web UI (area: web-ui)

### types.ts additions

```ts
export type SessionMode = "local" | "project";
export type OrbStatus = "" | "building" | "starting" | "running" | "stopped" | "failed";
export interface OrbSummary { slug: string; image: string; built: boolean; build?: string; error?: string }
export interface Project { id: string; name: string; slug?: string; orb?: OrbSummary }   // extended
export interface Row { /* existing */ mode?: SessionMode; orb?: { project: string; status: OrbStatus } }
export interface OrbState { session: string; project: string; status: OrbStatus; image?: string; container?: string; worktrees?: Record<string, string>; primary?: string; error?: string; updatedAt: string }
export interface OrbBuild { tag: string; hash: string; state: "" | "building" | "ok" | "failed"; startedAt: string; endedAt?: string; error?: string }
export interface OrbDetail { project: Project; files: Record<"project.yml" | "Dockerfile" | "setup.sh" | "resume.sh", string>; hash: string; orb: OrbSummary; build: OrbBuild; orbs: OrbState[]; runtime: { name: string; available: boolean; error?: string } }
```

`Ask` (existing) gains `secret?: boolean`: the card shows its own password
field that calls `api.answer` directly, the composer is disabled, and an
`ask/answer` line with `data.secret` renders `secret stored`.

`api.ts`: `create(cwd, prompt, mode?: SessionMode, project?: string)`,
`attachOrb(id, slug?)`, `detachOrb(id)`, `orb(id)`, `putOrbFile(id, name, text)`,
`buildOrb(id)`, `buildLog(id, offset)`, `sessionOrb(id)`, `stopOrb(id)`.

### Surfaces (sidebar and every existing view stay)

1. **New-session mode picker** — `ModePicker` in new `src/mode.tsx`:
   props `{projects: Project[]; value: {mode: SessionMode; project?: string}; onChange}`.
   A `.ctl` segmented pair "Local" / project select listing only projects
   with `slug`; mounted in the composer `.controls` row of an empty/new
   thread (where the palette's new session lands) above the first prompt,
   and in the palette's new-session path. Default Local. Passing it to
   `api.create` is the only wiring change in app.tsx.
2. **Mode chip** — `ModeChip` in `mode.tsx`, props `{row: Row}`: renders
   nothing for local; for project `<span class="status mono mode-chip">slug · status</span>`
   coloured by status (running `--accent`, building/starting `--amber`,
   failed `--red`, stopped `--text-3`). Mounted in `.row-meta` of sidebar
   rows and in `.thread-head` next to the existing `.status`. Thread head
   also gets a Stop orb `.btn` when running.
3. **Project orb detail** — `ProjectOrb` in new `src/orb.tsx`, props
   `{project: Project; detail?: OrbDetail; log: string; onAttach; onDetach; onSave(name, text); onBuild; onStopOrb(session)}`.
   Mounted inside `ProjectsView` (projects.tsx): each `.proj` section's
   `.proj-head` gains an "Orb" `.link` (or "Add orb" when no slug) that
   expands the section into `.proj-orb`: runtime line, image tag + built
   state, tabs (plain `.btn`s) for the four files with a `<textarea
   class="field mono orb-editor">` and Save `.btn-primary`, a Build
   `.btn`, the build log in a `.block` with `.block-lines` (auto-scroll,
   polled while building), and the orbs table as `.proj-row`s linking to
   sessions. Container fetching lives in ProjectsView, not ProjectOrb.
   Hash route `#/projects/<id>/orb` opens it expanded.

### New CSS (edit the `<style>` in dist/index.html only, then `bun run design:sync`)

`.mode-chip`, `.mode-running`, `.mode-busy`, `.mode-failed`, `.mode-stopped`
(colour tokens only), `.proj-orb` (grid, gap, `--surface`), `.orb-editor`
(min-height 16rem, full width, `--raised`, `--mono`), `.orb-tabs`
(flex wrap). Under the existing `<720px` media block: tabs and buttons 44px
tall, editor full width, log horizontal scroll inside the block.
Stories: `src/stories/orb.stories.tsx` (ProjectOrb: no orb, editing, building
with log, failed) and `mode.stories.tsx` (ModePicker, ModeChip each status);
export `ModePicker`, `ModeChip`, `ProjectOrb` from `ds-entry.ts`. Add a
Projects-family line to `.design-sync/conventions.md` for the new classes.

## 5. `bough update` (area: update)

New file `cmd/bough/containerrt.go`:

```go
// ensureContainerRuntime installs and starts Apple's container CLI on
// darwin. It warns and returns; it never fails the update.
func ensureContainerRuntime(out io.Writer, run func(name string, args ...string) error, look func(string) (string, error), goos string)
```

- non-darwin: no-op.
- `look("container")` and `/opt/homebrew/bin/container` both missing:
  `look("brew")` missing => warn "install Homebrew then `brew install container`";
  else `run("brew", "install", "container")`, failure => warn.
- then `run(bin, "system", "status")`; non-zero => `run(bin, "system", "start", "--enable-kernel-install")`
  (the flag is UNVERIFIED — `system start --help` only prints once the
  system runs; on non-zero retry plain `system start`, which may prompt
  for the kernel download, so stdin must be /dev/null and a hang is
  bounded by a 5-minute context); failure => warn.
- In tests `run`/`look` are fakes; nothing here may call the real
  `container` binary (the user's machine has the system stopped).
- Called from `runUpdate` after the build step and before `restartWeb`, as
  `ensureContainerRuntime(os.Stdout, runQuiet, exec.LookPath, runtime.GOOS)`.
- Warnings read `bough: container runtime: <what> (<fix>)`.

## 6. Test plan

All tests `t.Parallel()`, own `t.TempDir()` HOME (`t.Setenv` is not
parallel-safe: pass home explicitly — every function above takes `home`),
offline. Fake runtime everywhere except the one live test.

- **runtime**: Fake behaviour (Start/Inspect/Stop/Remove/ImageExists,
  Command runs on host in Workdir); Apple argv construction tested by
  injecting a fake `Bin` script that records argv to a file; Linux stubs
  return `ErrNotImplemented`; `apple_live_test.go` skipped unless
  `BOUGH_LIVE_CONTAINER=1`: build a 2-line Dockerfile, start, exec
  `echo ok`, bind-mount write visible on host AND owned by the host user,
  exec does not see an unpassed host env var, killing the exec client
  ends the guest process, named volume mount works, stop, start again,
  remove. It uses its own image/container names (`bough-orb-livetest-*`)
  and removes only those.
- **projectdef**: Parse/validate (no repos, bad slug, Dockerfile+setup);
  WriteFile atomic + delete-on-empty; ImageHash stable, changes on each
  input (Dockerfile, yml, lockfile at branch head), unchanged on unrelated
  repo file; orb.EnsureImage with Fake (builds once, second call no-op,
  concurrent calls build once via lock, failed build writes build.json
  failed + log); orb.Open with local git repos made in TempDir (worktrees
  on `bough/<sid>`, reuse on second Open, resume.sh failure => Failed but
  usable); Remove cleans worktrees.
- **session-mode**: main flag/env/resume precedence (table test on a pure
  `resolveMode` func); history meta has mode/project, old meta => local,
  no checkpoints in local; tools: local context lacks write/patch and has
  section `mode`; project context with a fake `orb` service routes bash and
  a background job through it and refuses write outside Root; project
  context WITHOUT `orb` makes bash error (never runs on host); orb row
  injects `scratch` (mount test with the real scratchpad row); orb row with
  `runtime: fake` calls the swapped chdir with primary and writes
  state.json; EnsureImage waiter does not truncate build.log; headless e2e
  (`--project` + fake runtime) runs `tools.bash("pwd")`.
- **serve-api**: httptest over a Supervisor with Fake runtime and a stub
  exe: orb detail, PUT files (400 on bad yaml), build 202 → log poll
  reaches ok, 409 while building, sessions row `mode`/`orb` from a written
  state.json, dead-PID => stopped, create with mode=project sets env
  (assert via the stub child echoing its env) and files the session under
  the slug, and a version-1 meta.json migrating to directories.
- **web-ui**: `bun run build && bun run check`; stories render each state;
  existing projects stories unchanged; phone width checked in Storybook at
  400px.
- **secrets** (docs/secrets.md): `internal/secrets` through the
  `KeychainRead`/`KeychainWrite` seams, never the real keychain;
  projectdef secrets validation, ImageHash ignoring secrets, SetSecret
  file bytes free of the value; `internal/orb` exec env carries a resolved
  secret for Command and resume.sh but not RunSpec env; ask/ui/serve
  redaction tests; `plugins/orb` `TestMissingEnv` (defaults, single
  quotes, lowercase, assigned, provided names) and the prompt section's
  rule and unset-env line.
- **update**: ensureContainerRuntime with fake run/look: darwin missing →
  brew install + system start; brew missing → warning only; start fails →
  warning, no panic; linux → no calls.

Gates for every area: `gofmt -l` empty; `go vet ./...`;
`go test -race -parallel 4 ./... 2>&1 | grep -E '^(FAIL|--- FAIL|panic)'`
must print nothing.

## 7. Area ownership

Every touched file has exactly one owner. Others needing a change in it
send the owner the exact lines (listed under "needs").

| file | owner |
|---|---|
| go/internal/container/runtime.go, apple.go, linux.go, fake.go, *_test.go, apple_live_test.go | runtime |
| go/internal/projectdef/projectdef.go, hash.go, *_test.go | projectdef |
| go/internal/orb/orb.go, state.go, image.go, worktree.go, lock_unix.go, lock_other.go, *_test.go | projectdef |
| go/plugins/orb/orb.go, orb_test.go | session-mode |
| go/plugins/history/history.go, tree.go, search.go, history_test.go | session-mode |
| go/plugins/tools/tools.go, jobs.go, tools_test.go, mode_test.go | session-mode |
| go/cmd/bough/main.go, cli_test.go, mode.go, mode_test.go | session-mode |
| go/bough.yml | session-mode |
| go/README.md (service-key table, CLI table) | session-mode |
| go/e2e/orb_test.go | session-mode |
| go/internal/serve/supervisor.go, api.go, projects.go, status.go, orbs.go, orbs_test.go, api_test.go, projects_test.go, supervisor_test.go | serve-api |
| go/cmd/bough/serve.go | serve-api |
| go/internal/serve/web/src/types.ts, api.ts, app.tsx, projects.tsx, palette.tsx, mode.tsx, orb.tsx, ds-entry.ts, stories/orb.stories.tsx, stories/mode.stories.tsx, stories/fixtures.ts | web-ui |
| go/internal/serve/web/dist/index.html, dist/app.js (build output), design/* (sync output), .design-sync/conventions.md | web-ui |
| go/cmd/bough/update.go, containerrt.go, containerrt_test.go, update_test.go | update |
| go/docs/orbs.md | architect (this file) |

Needs across areas:
- session-mode needs in main.go: blank import `_ "github.com/andreylukin/bough/plugins/orb"` (own file, listed so nobody else adds it).
- serve-api depends on `container.Fake`, `projectdef.*`, `orb.ReadState/ReadBuild/EnsureImage/ImageLogPath/Remove` exactly as declared in §1; until they land, code against the signatures (stub locally in a `_test` file only, never a second package).
- session-mode depends on `orb.Open/(*Orb).Command/State/Stop` and `container.Default/Fake`.
- web-ui depends only on §3 JSON; use fixtures until serve-api lands.
- update depends on nothing.
- If an area finds a signature here unworkable, it changes only its own
  implementation shape and reports the diff to the architect; §1/§3 names
  do not move without every dependent area agreeing.
