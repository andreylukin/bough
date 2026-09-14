# Examples

Everything below works on a stock install. The model in every example is `openai/gpt-6-astra` via OpenRouter; any provider works.

- [The plugin tree](#the-plugin-tree)
- [What the model writes](#what-the-model-writes)
- [init.js: tools, commands, providers](#initjs-tools-commands-providers)
- [Hooks](#hooks)
- [Skills, rules and the off switch](#skills-rules-and-the-off-switch)
- [MCP](#mcp)
- [The LLM wiki](#the-llm-wiki)
- [Scripting bough](#scripting-bough)
- [Replaying a session](#replaying-a-session)

## The plugin tree

bough follows [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness): a small kernel, and everything else is a plugin. `bough.yml` is a list of rows. Each row names a plugin and its config; rows find each other only through service keys (`llm`, `codemode`, `history`, …), and a row mounts once the services it needs exist.

`~/.bough/bough.yml` only lists what you change. This one picks a model, adds a small model for titles and activity lines, and turns the browser row off:

```yaml
- id: llm
  plugin: llm-openrouter
  config:
    model: openai/gpt-6-astra
- id: llm-small
  plugin: llm-openrouter
  config:
    service: llm-small
    model: openai/gpt-5.6-luna
- id: web
  plugin: web
  disabled: true
```

See the live tree, including rows waiting on a dependency:

```sh
bough rows
```

Save `bough.yml` while a session runs and bough reconciles only what changed. Swap the `llm` row mid-conversation and the loop remounts on the new provider; the conversation survives because the model context is rebuilt from the session log. A file that fails to parse keeps the last good tree.

One-off overrides without editing anything:

```sh
bough --set llm.model=deepseek/deepseek-chat          # change a row's config
bough --set llm.plugin=llm-echo                       # swap a row's plugin
```

## What the model writes

The model acts by writing JavaScript. Each `tools.*` call is a plain function, so it can batch, loop and branch:

```js
// Read every file that mentions the symbol, then patch and prove it in one step.
const hits = tools.bash("rg -l 'func TopN' .").trim().split("\n");
for (const f of hits) console.log(tools.view(f));
console.log(tools.patch("wordfreq.go", "return all[:n]", "if n > len(all) {\n\t\tn = len(all)\n\t}\n\treturn all[:n]"));
console.log(tools.bash("go test ./... 2>&1 | tail -5"));
```

Subagents are a function call too. `tools.spawnAll` runs bounded child agents in parallel and returns their final replies:

```js
const [api, ui] = tools.spawnAll([
  "List every HTTP route in internal/serve/api.go with its handler name.",
  "List every fetch() call in internal/serve/web/src with the path it hits.",
]);
console.log(api, "\n---\n", ui);
```

Pass a JSON Schema and each child's report is checked against it and comes back as an object the program can index:

```js
const routes = tools.spawnAll(
  ["Routes in internal/serve/api.go", "Routes in internal/serve/web.go"],
  {type: "object", required: ["routes"],
   properties: {routes: {type: "array", items: {type: "string"}}}},
);
console.log(routes.flatMap((r) => r.routes).sort().join("\n"));
```

`tools.ask` stops and asks you, with options rendered as buttons in the TUI and the web control room:

```js
const choice = tools.ask("Treat 'Go' and 'go' as the same word?", "same", "different");
```

`tools.artifact` publishes a page (a table, a chart, a form) that bough serves locally for you to read or fill in; `tools.artifactAnswers(name)` reads back what you entered.

## init.js: tools, commands, providers

`~/.bough/init.js` (and `./.bough/init.js` per project) runs at boot in the same JavaScript runtime the model uses. The full API is in [go/docs/INIT.md](go/docs/INIT.md).

```js
bough.setup({
  ui: {theme: {user: "#87d7ff:bold"}, keymap: {quit: "ctrl+q"}},
  system: {append: "Prefer ripgrep over grep. Never push to git remotes."},
});

// A tool the model can call as tools.today()
bough.tool("today", () => tools.bash("date +%F").trim());

// A slash command: /budget
bough.command("budget", "", "spend so far", () => {
  const s = bough.session();
  return "$" + s.usage.cost.toFixed(3) + " over " + s.turns + " turns on " + s.model;
});

// A whole LLM provider in a few lines (used when setup.provider.default names it)
bough.provider("parrot", (system, messages) => "You said: " + messages[messages.length - 1].content);
```

## Hooks

A hook is a `.js` file whose body runs on an event, re-read on every fire, so you edit it live. Put it in `~/.bough/hooks/<event>/` or `./.bough/hooks/<event>/`.

Refuse destructive commands before they run (`pre-code-exec`):

```js
// ~/.bough/hooks/pre-code-exec/no-force-push.js
// Description: Block force pushes and recursive deletes of home.
if (/git\s+push\s+.*--force|rm\s+-rf\s+~/.test(event.code)) {
  return {deny: "force pushes and rm -rf ~ are off limits"};
}
```

The whole block is skipped, and the model sees `[hook denied: force pushes and rm -rf ~ are off limits]` as the result and tries something else.

Rewrite every prompt (`user-prompt-submit`):

```js
// ~/.bough/hooks/user-prompt-submit/terse.js
// Description: Ask for terse replies.
return {input: event.input + "\n(reply tersely)"};
```

Other events: `session-start` (return `{context}` to add to the system prompt), `post-result` (rewrite what the model sees), `stop`, `session-end`.

## Skills, rules and the off switch

bough reads what Claude Code and Codex users already have:

- **Skills** in `~/.claude/skills/<name>/SKILL.md`, `~/.bough/skills` and `./.claude/skills`: mention a skill's name in a message and its instructions join that turn. Each is also a `/name` command.
- **Plugins** installed for Claude Code: their skills and commands are picked up.
- **Rules** in `.claude/rules/` (a `paths:` glob scopes a rule to matching files) and `.codex/rules/`.
- **Context files**: `AGENTS.md`, `CLAUDE.md`, `~/.bough/BOUGH.md`.

Everything discovered is on by default. `~/.bough/off.yml` turns single items off:

```yaml
disabled:
  - skill:circleci
  - hook:post-result/audit.js
```

## MCP

Servers come from `./.mcp.json`, `~/.claude.json`, or the `mcp` row, and every tool becomes `tools.mcp_<server>_<tool>` inside the model's program:

```json
{"mcpServers": {"github": {"command": "github-mcp-server", "args": ["stdio"]}}}
```

```sh
bough mcp list                 # servers and status
bough mcp search "pull request"
bough mcp call github/list_pull_requests '{"owner":"andreylukin","repo":"bough"}'
```

## The LLM wiki

bough keeps two levels of record. `~/.bough/history/` is the append-only log of every session. `~/.bough/wiki/` is markdown pages an agent compiles from that log: decisions and why, root causes, gotchas, where things live. Every claim cites the log entry it came from as `` `<session>#<seq>` ``, so any line can be checked against what actually happened. Nothing from the wiki is injected into prompts; an agent reads it when asked, or greps it like any file.

```sh
bough wiki install     # every 5 minutes, ingest sessions quiet for 30 minutes (launchd)
bough wiki pending     # what is waiting to be compiled
bough wiki check       # every citation resolves, every link and index entry exists
```

In a session: `/llm-wiki query why did we drop the Rust rebuild?`. In `bough serve`, the Wiki view shows each page as claims beside their evidence, flags uncited or unresolvable ones for review, and lists every ingest with its cost. The wiki is its own git repo, committed after each ingest.

## Scripting bough

`--headless` reads prompts from stdin and prints events; `--json` gives one object per line:

```sh
echo "Summarize what changed on this branch in three bullets." | bough --headless --json \
  | jq -r 'select(.kind=="assistant") | .text'
```

A multi-line brief is one JSON line:

```sh
jq -nc --rawfile brief TASK.md '{prompt: $brief}' | bough --headless
```

`bough serve` runs sessions as headless children behind a local API and web UI, and `bough search "rate limit repo:bough since:7d"` finds sessions by what was said in them.

## Replaying a session

Any recorded session plays back through the real loop and TUI with no model and no shell, which makes every session a free, deterministic test case:

```yaml
- id: llm
  plugin: replay
  config: {session: 01a0…}
- id: codemode
  plugin: replay
  config: {session: 01a0…}
```
