# Secrets for project sessions

Contract for three parallel work areas. Paths are relative to `go/`.

Why: a project session could not run its tests because it had no
`DEVPI_URL`. The value lives only in the user's password manager, and the
orb passes a fixed env allowlist, so the agent quietly fell back to lint.
This spec makes secrets available by reference, lets the agent ask the user
for one, and makes a blocked check visible.

## 1. project.yml schema

```yaml
secrets:
  DEVPI_URL: keychain:bough/example-app/DEVPI_URL
```

- `Def.Secrets map[string]string \`yaml:"secrets,omitempty"\``. Each key is an
  env name and each value is a ref.
- Validation happens in `Parse`, after the cpus check:
  - The name must match `^[A-Za-z_][A-Za-z0-9_]*$`.
  - The ref must be `keychain:<service>`. The service must be non-empty,
    at most 200 bytes, and contain no whitespace or control characters.
    Any other scheme is an error: `secrets.NAME: unknown ref scheme "x" (want keychain:)`.
  - A name can't be in both `env` and `secrets`
    (`secrets.NAME: also set in env`).
  - A name can't shadow what every exec sets (`projectdef.ReservedEnv`
    plus the `BOUGH_` and `GIT_CONFIG_` prefixes):
    `secrets.NAME: reserved env name (set by every exec)`.
- Values never live in project.yml. `bough project show` prints the file,
  so it shows refs only, and there is nothing to redact.
- `ImageHash` has to ignore `secrets`: it hashes project.yml with the
  `secrets` key removed (parse, clear `Secrets`, re-marshal). Otherwise every
  new secret would rebuild the image. The re-marshal normalizes every
  project.yml, so each existing orb image rebuilds once after upgrading.
  Secrets also never reach
  setup.sh or the Dockerfile build.

## 2. Resolution, caching, test seam

Package `internal/secrets` (new, area A):

```go
// Read returns the value behind ref. ErrNotFound when the item is missing.
func Resolve(ref string) (string, error)
// Store writes value to the keychain under service (create or update).
func Store(service, value string) error
// Service is the conventional service for an asked secret.
func Service(slug, name string) string // "bough/<slug>/<NAME>"
// Ref is "keychain:" + service.
func Ref(service string) string
var ErrNotFound = errors.New("secret not found")

// Seams: tests swap these and never touch the user's keychain.
var KeychainRead  = func(service string) (string, error)   // security find-generic-password -s <service> -w
var KeychainWrite = func(service, value string) error      // see below
```

- Read runs `/usr/bin/security find-generic-password -s <svc> -w` (any
  account, so a ref can name an item another tool created) with a 30s
  timeout, which leaves time for a keychain prompt. Exit 44 maps to
  `ErrNotFound`, wrapped with the service name. Any other failure is returned with stderr, and
  the value never appears in the error.
- Write runs `/usr/bin/security -i` and passes
  `add-generic-password -U -a bough -s "<svc>" -w "<value>"\n` on **stdin**,
  so the value never shows up in argv or `ps`. `Store` rejects a value that
  contains `"`, `'`, `\`, a newline or NUL with
  `secrets: value has unsupported characters`. Don't use `-A`.
  `security -i` exits 0 when the subcommand fails, so the write is
  confirmed by reading the value back.
- `BOUGH_TEST_KEYCHAIN_DIR=<dir>` swaps both seams for one plaintext file
  per service, so a live run of the binary never touches the login
  keychain. Test use only.
- Cache: `Resolve` caches per ref for 5 minutes, the same way `githubToken`
  does. A failure is cached for 30s, so a locked keychain does not stall
  every exec on the timeout. `Store` drops that service's cache entry.
- `projectdef.SetSecret(home, slug, name, ref string) error` (area A): load
  the project, set `Def.Secrets[name]`, validate, and write it atomically
  through `WriteFile`. It deletes `Def.Env[name]` if that is set. B calls
  this function.

## 3. Per-exec env

`(*Orb).execEnv` builds the env in this order. When a name appears twice,
the later value wins:

1. coreEnv (`HOME`, `TERM`, `BOUGH_SCRATCH`)
2. identityEnv (`GH_TOKEN`, `GIT_CONFIG_*`, host prefixes)
3. proxyEnv, `BOUGH_HOST`, `PATH`
4. `envList(Def.Env)`
5. **resolved secrets, sorted by name**

- Refs are re-read on every exec from `projectdef.Load(home, slug)`
  (cheap, one file). A secret added mid-session therefore applies to the next
  `tools.bash` call without reopening the orb. If loading fails, fall back to
  the `Def` captured at Open.
- A ref that fails to resolve leaves its name out of the env. The first
  failure per name per orb goes to stderr:
  `bough: orb: secret NAME unresolved: <err>`.
- Secrets never go into `baseEnv` or `RunSpec.Env`, because inspect shows
  those.
- Both `Orb.Command` (tools.bash, background jobs, and so the agent's
  checks) and `runResume` (resume.sh) call execEnv, which covers
  requirement 3. The test is `internal/orb` with fake runtime + fake
  `KeychainRead`: set a ref, then assert the value is in the exec env for
  resume and for Command, and not in the RunSpec env.
- resume.log is on the host. A script that echoes a secret leaks it there.
  The /orb skill warns about this, and bough does not scrub it.

## 4. Asking for a secret

**Choice: a codemode tool, `tools.secret(name, reason[, project])`, in
plugins/ask.** It isn't a CLI command. The guest relay runs `bough project`
as a separate host process that has no kernel, no ask-answers service and
no session, and local sessions have no relay at all. Tools run in the host
bough process for both local and project sessions, so a tool reaches the
existing ask UI (TUI directly, serve through the headless ask event)
without new plumbing. The CLI only gets `set <slug> secrets.NAME <ref>`,
for pointing at an item the user already has.

```
tools.secret("DEVPI_URL", "uv needs the devpi index to install test deps")
  -> "stored DEVPI_URL as keychain:bough/example-app/DEVPI_URL; available to the next command"
```

- `project` defaults to `session-project`. In a local session, or when it
  is empty, the tool throws `secret: pass the project slug`. The project
  must exist (`projectdef.Load`).
- Flow:
  1. `Asker.askSecret` validates the name.
  2. It asks the question `Secret NAME for <slug>: <reason>` with no options
     and `Secret: true`.
  3. On an answer it calls `secrets.Store(secrets.Service(slug,name), value)`,
     then `projectdef.SetSecret(home, slug, name, secrets.Ref(service))`.
  4. It returns the string above. It never returns the value.
- Errors are returned without the value: `secret: store failed: <err>`.
- Declining (esc sends `(declined)`, or a cancel) throws
  `secret: user declined`, and nothing is stored. An empty answer counts as
  declined.
- Describe line:
  `tools.secret(name, reason, project?) asks the user for a credential, stores it in the keychain and adds it to project.yml secrets. The value is not returned to you, but commands see it as env, so never print it.`

### Asker changes (plugins/ask/ask.go)

- `Event` gets `Secret bool`, and `ui.Event` gets the same field, which
  `eventOf` reads by reflection.
- `pending` becomes a map of `{ch chan string; secret bool}`.
- The history `ask` entry is `{question, options, id, secret:true}`.
- `Answer(id, text)`: for a secret pending ask, append `ask/answer`
  `{id, text:"[secret stored]", secret:true}`, then pass the raw text to the
  channel. This is the only place the raw value moves, and it reaches the
  askSecret goroutine only.
- `Ask` (the exported entry for rules) is unchanged.

### Redaction points and tests

| Where | Rule | Test |
|---|---|---|
| history `ask/answer` | text `[secret stored]`, `secret:true` | ask_test: answer a secret ask, and the history fake has no raw value |
| tool result / llm | return value and errors contain no value | ask_test: returned string and thrown error don't contain the value; the loop projects only `result` |
| export, wiki digest, EntryText, serve Line | fixed by the history entry | ask_test covers it; grep the export output in a commands test |
| TUI | while the pending ask is secret, the composer renders `•` per rune (a render override, because textarea has no echo mode). `answerAsk` sets `b.answer = "[secret stored]"`. The text never enters askStash, input history, predict or voice | ui test: model with a secret ask, type a value, Enter, and neither View() nor history recall has it |
| headless | `hlPrint` adds `"secret":true` to the ask extra. While the pending ask is secret, `hlLineIn` passes the raw line to `hlAnswerPending` with no JSON sniffing, and it never prints the line | headless test |
| serve | `askFrom`/`askOf` copy `secret`, and `status.Ask` gets `Secret bool \`json:"secret,omitempty"\``. `Answer` logs nothing, and it rejects text with a newline (`ErrBadAnswer`) | status_test on a secret ask entry |
| web | `Ask.secret?: boolean`. For a secret ask the card shows its own `<input type="password" autocomplete="off">` and submit button that call `api.answer` directly. No draft-ask sessionStorage, no Failure entry holding the text (a failure shows "not sent, try again" with an empty field), and the field is cleared after posting. The composer is disabled with "Answer in the secret field". The `ask/answer` line renders `secret stored`, never `line.text`, when `data.secret` is set | web spec if present; otherwise a manual check in the `serve --run` preview |
| `bough project show` | refs only, because values aren't in files | projectdef test: after SetSecret, the file bytes don't contain the value |

A secret ask inside a loop pipeline agent is killed like any other ask. That
is acceptable.

## 5. Prompt rule (plugins/orb promptSection, verbatim)

> If a build or test is blocked by a missing credential, dependency or tool, do not fall back to weaker verification. Find what the repo expects (Makefile, docker-compose, CI config), fix the project definition with `bough project`, ask for secrets with `tools.secret`, and say plainly what stayed unverified.

## 6. Startup check

- `plugins/orb/envcheck.go`:
  `func missingEnv(resume string, checks projectdef.Checks, def projectdef.Def) []string`
  (pure, sorted, unique).
- Scan: in resume.sh plus checks.fast and checks.full, match `\$\{?([A-Za-z_][A-Za-z0-9_]*)`.
  Skip:
  - `${NAME:-...}`, `${NAME-...}`, `${NAME:=...}` and `${NAME=...}`, since
    they have defaults.
  - Anything inside single quotes.
  - Lowercase-only names.
  - Names assigned in the script: `NAME=`, `export NAME=`, `read NAME`,
    `for NAME in`, `local NAME`.
- Provided names:
  - `def.Env` and `def.Secrets`.
  - Core: `HOME TERM BOUGH_SCRATCH BOUGH_HOST PATH`.
  - Identity: `GH_TOKEN`, and the prefixes `GIT_CONFIG_`, `AWS_`,
    `GRAFANA_`. Copy the prefixes from `identityEnvPrefixes`, and keep a
    unit test that asserts the two lists match via an exported
    `iorb.IdentityEnvPrefixes` if one exists. Otherwise hardcode the list
    and leave a comment.
  - Proxy: `HTTP_PROXY HTTPS_PROXY NO_PROXY` in upper and lower case,
    `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE`.
  - Shell: `PWD OLDPWD USER SHELL LANG LC_ALL TMPDIR HOSTNAME IFS PS1 RANDOM
    SECONDS LINENO UID EUID PPID OPTARG OPTIND CI`, positional and special
    parameters (`$1`, `$@`, `$?`, `$$`, `$#`, `$!`), and `BASH_*`.
- Surfaces:
  - The prompt section gets the line
    `Unset env referenced by resume.sh/checks: A, B. Set them (bough project set <slug> env.NAME / tools.secret) before trusting checks.`
  - The orb row writes one stderr notice
    `bough: orb: <slug>: unset env A, B (resume.sh/checks)`, the
    existing notice channel.
  - No new state field, which keeps area C out of internal/orb.
- `promptSection` signature becomes
  `promptSection(root string, st iorb.State, def projectdef.Def, missing []string) string`.

## 7. /orb skill outline (plugins/orb/SKILL.md, embedded and written to ~/.bough/skills/orb by the orb row, `manual: true`, script-free)

0. Install: `ln -s "$PWD/go/skills/orb" ~/.bough/skills/orb`.
1. Inspect: `bough project show <slug>`, then read each repo's Makefile,
   docker-compose, CI config (`.github/workflows`, `.circleci`), README,
   lockfiles and `.tool-versions` with tools.view or tools.bash.
2. Base image vs `setup.sh` vs `Dockerfile`: use the base image when the
   toolchain is apt- or curl-installable, and a Dockerfile only for a
   different distro.
3. `setup.sh` is build-time and gets no secrets: toolchains and system
   packages. `resume.sh` runs per session and has secrets available:
   dependency install (`uv sync`, `npm ci`) and devpi or index config.
   Never echo secrets, because resume.log is on the host.
4. Caches (`caches:` in project.yml) and env: `bough project set <slug> env.NAME value`.
5. Secrets: for each credential the repo expects (Makefile `ensure-*`
   targets, `.env.example`), call `tools.secret(NAME, reason)`, or
   `bough project set <slug> secrets.NAME keychain:<service>` for an item
   the user already has.
6. Checks: `bough project set <slug> checks.fast "<cmd>"` and
   `checks.full`. Use the commands CI runs.
7. Write the files with `bough project write <slug> setup.sh|resume.sh`
   (stdin).
8. Verify: definition changes to image or setup apply in the next session.
   Run resume.sh and checks.fast by hand through tools.bash now. Report
   what passed, what is unverified and why.

## 8. Docs to update (area C)

- `docs/orbs.md`:
  - §1b: add `Secrets` to `Def`, with validation, and note that
    ImageHash ignores secrets.
  - §1c: document the execEnv order.
  - §1d: add a Secrets bullet, and fix the stale "only `mcp`" relay line
    (it is `mcp` and `project`).
  - §2 orb row: the prompt rule and the missing-env line.
  - §2 Tools and History: `tools.secret` and redaction.
  - §4: the `Ask.secret` type.
  - §6: tests.
- `docs/loops.md` Area B skills list: add `orb`.

## Areas and file lists (disjoint)

**A: secrets core**
- internal/secrets/secrets.go, internal/secrets/secrets_test.go (new)
- internal/projectdef/projectdef.go, projectdef_test.go (Secrets, validation, SetSecret)
- internal/projectdef/hash.go (+ test): exclude secrets
- internal/orb/orb.go, internal/orb/orb_test.go (execEnv secrets, per-exec reload)
- cmd/bough/project.go (+ test): `set <slug> secrets.NAME <ref>` (empty ref removes it), usage and error text

**B: secret ask + redaction** (depends only on `secrets.Store`,
`secrets.Service`, `secrets.Ref` and `projectdef.SetSecret` as specified
above. To build before A lands, stub them locally and don't commit the
stubs.)
- plugins/ask/ask.go, ask_test.go
- plugins/ui/ui.go, ask.go, model.go, headless.go, session.go (+ tests)
- internal/serve/supervisor.go, status.go, api.go (+ tests)
- internal/serve/web/src/types.ts, app.tsx (+ rebuilt web dist)
- plugins/commands/export.go, plugins/wiki/digest.go: only if a test shows a leak, since the history entry already fixes both

**C: prompt, startup check, skill, docs**
- plugins/orb/orb.go, plugins/orb/envcheck.go (new), plugins/orb/orb_test.go, envcheck_test.go
- plugins/orb/SKILL.md (new; installed like plugins/wiki's llm-wiki skill)
- docs/orbs.md, docs/loops.md
