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
  repo. serve's existing label projects may point at one via a new `slug`
  field; label-only projects keep working exactly as today.

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

type CommitSpec struct {
	Base   string // generic base image, DefaultBase unless project.yml says otherwise
	Script string // host path of the setup script, copied into the build context and run with sh
	// Files are extra host files copied into the build context next to
	// the script (the repos' lockfiles, as <repo>/<lockfile>) so setup
	// can pre-install dependencies. No bind mounts: Apple 1.1.0 has no
	// commit verb, so Commit is a generated Dockerfile build, and a
	// build cannot see mounts.
	Files []string
	Env   []string // becomes ENV lines
	Tag   string
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

// ErrNotImplemented is what the Linux stubs return from every method.
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
`container run -d --name N -v SRC:DST[:ro] --mount type=volume,source=V,target=DST -w DIR -e K=V -c CPUS -m MEM IMAGE sleep infinity`;
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
(`FROM base`, `ENV`, `COPY . /bough-setup`, `RUN sh /bough-setup/setup.sh`)
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
	Caches  []string          `yaml:"caches,omitempty"`  // guest dirs backed by named volumes, e.g. /root/.cache/go-build
	Env     map[string]string `yaml:"env,omitempty"`
	CPUs    int               `yaml:"cpus,omitempty"`
	Memory  string            `yaml:"memory,omitempty"`
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

func Root(home string) string                     // home/.bough/projects
func ValidSlug(s string) error
func List(home string) ([]Project, error)         // dirs with a project.yml; bad yaml => entry skipped + returned in a joined error
func Load(home, slug string) (Project, error)
func Create(home, slug string) (Project, error)   // writes skeleton project.yml + setup.sh; exists => error
func ReadFile(home, slug, name string) (string, error)   // missing => "", nil
// WriteFile validates: name in EditableFiles; project.yml must parse and
// have >=1 repo; writing Dockerfile when setup.sh exists (or vice versa)
// is allowed and Dockerfile WINS at build time. Empty text deletes the
// file (project.yml cannot be deleted). Atomic temp+rename.
func WriteFile(home, slug, name, text string) error
func Parse(b []byte) (Def, error)

// ImageHash is sha256 over, in order: the Dockerfile OR setup.sh bytes
// (whichever is used), project.yml bytes, Base, and for each repo in
// order the bytes of any of LockfileNames found at the repo's root at
// its resolved branch head (read from the host source checkout /
// cached clone, never a worktree). Hex, first 12 chars.
func ImageHash(home string, p Project) (string, error)
var LockfileNames = []string{"go.sum", "package-lock.json", "bun.lock", "bun.lockb", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "poetry.lock", "uv.lock", "requirements.txt", "Gemfile.lock"}
func ImageTag(slug, hash string) string // "bough-orb/<slug>:<hash>"
```

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
// returns without touching build.log/build.json when a waiter finds the
// image already built; only the process that actually builds truncates
// and writes build.log; also tees to
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
// cache volumes "bough-cache-<slug>-<n>", Start with mounts
// {each worktree, scratchDir} at identical paths — plus, for Path repos,
// the source checkout's .git dir at its identical path, because a
// worktree's .git FILE points at <source>/.git/worktrees/<name> and git
// inside the container fails without it (remote repos: the cache .git
// dir likewise) — then resume.sh (if any)
// via Exec with output appended to Dir/resume.log. A resume.sh failure
// marks Failed but returns the Orb usable (the agent can fix it).
func Open(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratchDir string) (*Orb, error)
func (o *Orb) State() State
// Command is the exec seam: argv run inside the container, workdir =
// Primary, env = BOUGH_SCRATCH/HOME/TERM + Def.Env (exec does not
// inherit the host env). If the container is not running (stopped from
// serve, engine restarted) Command first re-Starts it and re-runs
// resume.sh, so a stop never strands a live child.
func (o *Orb) Command(ctx context.Context, argv ...string) *exec.Cmd
// Root is Dir(home, session): the directory holding every worktree.
func (o *Orb) Root() string
func (o *Orb) Stop(ctx context.Context) error   // container stop; state Stopped; worktrees kept
func Remove(ctx context.Context, rt container.Runtime, home, session string) error // rm container, git worktree remove, rm Dir
```

### 1d. Identity, tools and egress

A project session acts as the user, with the same permissions as their
own shell; it only isolates file changes.

- **Tools**: `projectdef.BaseDockerfile` (embedded `base.Dockerfile`) is
  built as `projectdef.BaseTag()` = `bough-orb/base:<sha12>` by
  `orb.EnsureBase` before any setup-script project whose `base` is empty.
  It carries gh, aws, kubectl, helm, helmfile, sops, just, gcx, argocd,
  uv and python3. `ImageHash` hashes the base tag, so editing the base
  rebuilds every project on it.
- **File credentials**: `~/.aws`, `~/.kube`, `~/.config/gcx`,
  `~/.config/argocd`, `~/.config/gcloud` are bind-mounted read-write at
  `/root/...` when present, so SSO and kube token caches stay shared with
  the host.
- **Keychain credentials**: `GH_TOKEN` is the host's `gh auth token`
  (cached 5 min), passed per exec, never in run env. Git gets
  `credential.https://github.com.helper=!gh auth git-credential` and the
  host's user.name/email through `GIT_CONFIG_*` env; the host gitconfig is
  not mounted (it names macOS binaries). Host `AWS_PROFILE`/`AWS_REGION`
  and `GRAFANA_*`/`ARGOCD_*`/`CIRCLECI_*`/`LINEAR_*` pass through.
- **Egress**: per-app VPNs (Jamf Trust Private Access) do not tunnel the
  VM bridge, so the child runs an HTTP/CONNECT proxy on the guest's
  gateway IP (its resolv.conf nameserver) and sets `HTTPS_PROXY` and
  friends per exec. Stop closes it; a restart reopens it.

## 2. Session mode (area: session-mode)

### Choosing the mode

- CLI: `bough --project <slug>` => project; `bough --local` (explicit,
  optional) => local; neither => local. Both => exit 2. Works with
  `--headless`, TUI and `-p`.
- Env (serve → children, and a person's shell): `BOUGH_MODE=local|project`,
  `BOUGH_PROJECT=<slug>`. Flags win over env. main reads then
  `os.Unsetenv`s both (same reason as BOUGH_ORIGIN: the agent's own bough
  runs must not inherit it).
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
  host"). Effect on unmount: `Stop` (never Remove — resume reuses it).
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
  `~/.bough/projects/<slug>` (the agent may edit its own project
  definition) with an error naming the allowed roots; same "not ready"
  error when `orb` is absent.
- Mode (`session-mode`, provided by main before any row mounts) is read
  once at Apply to decide registration; the `orb` service is not.

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

`meta.json` persists `Slug` inside projects. On load, every
`projectdef.List` slug with no label gets a label `{Name: slug, Slug: slug}`
(so a definition the agent created shows up). Deleting a label never
deletes `~/.bough/projects/<slug>`.

### Endpoints

| method + path | request | response |
|---|---|---|
| `GET /api/projects` | — | `{"projects": [Project & {"orb"?: OrbSummary}]}` (orb present iff slug) |
| `POST /api/projects/{id}/orb` | `{"slug": "..."}` (optional; default slugified name) | `{"project": Project, "orb": OrbSummary}`; creates skeleton via projectdef.Create or attaches an existing dir; 400 bad slug, 409 slug attached to another label |
| `DELETE /api/projects/{id}/orb` | — | `{"ok": true}`; detaches (clears Slug), files kept |
| `GET /api/projects/{id}/orb` | — | `OrbDetail` below; 404 no slug |
| `PUT /api/projects/{id}/orb/files/{name}` | `{"text": "..."}` | `{"ok": true, "orb": OrbSummary}`; name ∈ project.yml, Dockerfile, setup.sh, resume.sh; 400 with parse error |
| `POST /api/projects/{id}/orb/build` | `{}` | 202 `{"build": Build}`; runs orb.EnsureImage in a goroutine; 409 while building |
| `GET /api/projects/{id}/orb/build/log?offset=N` | — | `{"text": "...", "offset": M, "state": "building|ok|failed|"}` — bytes from N of build.log, max 256 KiB per call; client polls every 1 s while building |
| `GET /api/sessions/{id}/orb` | — | `{"orb": OrbState}` (`orb.State` JSON, status forced to `stopped` when PID dead and status was running/starting); local => `{"orb": null}` |
| `POST /api/sessions/{id}/orb/stop` | — | `{"ok": true}`; runtime Stop on OrbName. Allowed while the child is live: the child's `Orb.Command` re-starts a stopped container on the next exec. serve does NOT write state.json (the child is its only writer); it reports `stopped` from `Inspect` until the child rewrites it |
| `POST /api/sessions` | `{"cwd", "prompt", "mode"?: "local"\|"project", "project"?: "<label id>"}` | unchanged `{"session": Row}`. mode omitted => local. project mode: label must have a Slug (400 otherwise), cwd defaults to home and is ignored for the child dir; child env gets `BOUGH_MODE=project BOUGH_PROJECT=<slug>`, and the new session is auto-assigned to that label |

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
  exe: attach/detach orb, PUT files (400 on bad yaml), build 202 → log poll
  reaches ok, 409 while building, sessions row `mode`/`orb` from a written
  state.json, dead-PID => stopped, create with mode=project sets env
  (assert via the stub child echoing its env) and auto-assigns label,
  label-only projects and old meta.json unchanged (golden).
- **web-ui**: `bun run build && bun run check`; stories render each state;
  existing projects stories unchanged; phone width checked in Storybook at
  400px.
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
