# Working in this repo (the `rebuild` branch)

`REQUIREMENTS.md` is the spec and the only authority. Section numbers below refer to it. Read the
sections named by your task before touching code; when code and REQUIREMENTS disagree, REQUIREMENTS
wins and the code is the bug. `BUILD.md` is the phase ledger: what is done, what is verified, what
was deliberately deferred.

## Layout (§13, §17 Phase 0)

```
crates/bough-kernel/   the center: contexts, typed services, fibers, effects, events, isolate/
                       intercept, scope, loader (entries/group/include, per-field reconcile),
                       patch layers, invariant runner. NO domain vocabulary. §0.1, §0.3
crates/bough-util/     branded ids, home paths, timeouts. A library, no ctx key.
crates/bough-llm/      KEPT from the old tree: LlmClient over Anthropic/OpenAI/OpenRouter/... Do not
                       redesign it; wrap it (plugin llm-anthropic).
crates/bough/          the launcher: profiles, bundles, patch layers, --dump-config, fail-loud boot,
                       patch-file watch, teardown-before-exit. Composition only. §0.1, §0.5
plugins/<name>/        one crate per plugin row, package name `bough-plugin-<name>`; registers a
                       name + constructor through `inventory`; owns an `invariant` module. §9
bundles/               bough-base.yml, bough-tui-app.yml, bough-headless.yml (YAML patch lists)
profiles/              tui.yml, headless.yml, dev.yml (ordered bundle lists + profile patch)
scripts/               shell-use TUI scripts, audit-plugins.sh
docs/                  per-phase design notes written by the build (plan, decisions, deviations)
```

One cargo workspace at the root. `make gates` (build + lint + test) must be green before every commit.

## Rules the reviews enforce

- **Plugins, not loop changes** (§0.2). New behavior attaches to a service key or a typed event.
- **Registrations are effects**; every contribution returns a disposer; unload leaves no trace.
- **No hardcoded tunables in plugins**: a deployment-varying value is a validated `Config` field
  set from the bundle patch. Protocol constants and security invariants stay in code.
- **Misconfiguration fails loud.** An enabled row that never activates is a boot failure.
- **Every plugin crate has `src/invariant.rs`** checking an event stream or data relation it owns,
  or a `No runtime invariant:` statement with the reason.
- **Branded ids at boundaries**, explicit `resolve(request) -> Spec` for defaults, never `?? default`
  inside `run()`.
- **Model-visible ⟺ ledgered** (§0.2, §3). A new model-visible input is a new step type.
- Tests sit next to the module they cover; they are offline and hermetic. Anything needing a real
  model or the network is behind an env var (`BOUGH_LIVE=1`) and skipped otherwise.
- Every module opens with a comment stating the invariant it holds. Dependencies (db, clock, LLM
  client) are injected. Parsing and core logic are pure with `now` passed in.
- Every phase ends with a **swap test** (§17): one row introduced in the phase replaced or disabled
  by a patch, no compile, tree stays consistent.

## Verification tooling

- `shell-use` (on PATH) drives the TUI in a real PTY: `shell-use --help`. The Phase 3+ TUI scripts
  live in `scripts/tui/` and run under `make tui-test`.
- `gh` is the GitHub transport (§13: no octocrab). Tests never call the real `gh`: they put a
  recording shim first on PATH.
- Old data for the §14 adapter: `~/.bough/bough.db` (command_history, command_tags, note_sections);
  `~/.jungler/jungler.db` may be ABSENT on a machine and the adapter must activate anyway.

## Version control

Plain git, branch `rebuild`, worktree at `~/repos/bough-rebuild`. Commit per completed work package
with a message that names the REQUIREMENTS section it satisfies. Never `git stash`. Never `git add -A`
without reading `git status` first. Never touch `~/repos/bough` (the user's daily driver checkout).
