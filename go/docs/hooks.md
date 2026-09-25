# Hooks on the engine

Hook files are the same on both runners: `.js` bodies under
`~/.bough/hooks/<event>/` and `./.bough/hooks/<event>/`, run in the shared
codemode VM, merged in base-name order, with `deny` and `block`
short-circuiting and `notice` going to the human only. The event table in
[`go/README.md`](../README.md) is the loop's. This page is what changes when
the `loop` row runs `plugin: engine-unreal` (see [unreal-engine.md](unreal-engine.md) §11.4).

On the engine the model makes native tool calls instead of writing one
JavaScript block, so `pre-code-exec` and `post-result` fire **once per call**,
possibly for several calls at once. `code` carries the call's whole text:
bash's entire command, `run_js`'s entire program, and for `write` and `patch`
the path, a newline, then the text going into the file. Other tools (`view`
among them) get the call's detail, which for `view` is the path. That way a
hook that matches command text keeps matching wherever the text sits, as it
does against the loop's whole block, and the payload also names the tool.

| event                | when on the engine                        | payload                                   | result keys honoured |
|----------------------|-------------------------------------------|-------------------------------------------|----------------------|
| `session-start`      | the session's first model build           | `{}`                                      | `context` → frozen into the system prompt |
| `user-prompt-submit` | each line you send                        | `{input}`                                 | `block` → refuse the line; `input` → rewrite (shown to you) |
| `pre-code-exec`      | before each native call                   | `{code, tool, args, call}`                | `deny` or `block` (a string or `true`) → the model reads `Error: blocked by hook: <reason>`; `args` (an object) → replace the call's arguments |
| `post-result`        | after each native call                    | `{code, tool, call, result, error}`       | `result` → rewrite what the model reads |
| `stop`               | when a turn ends                          | `{reply}`                                 | `block` → the model is asked to continue with that text |
| `session-end`        | at unmount                                | `{}`                                      | none |

- `tool` is the native tool's name (`bash`, `write`, `mcp__linear__list_issues`, …).
- `args` is the call's arguments as an object. Arguments that do not parse as
  JSON arrive as the raw string.
- `call` is the provider's call id, the same id the call row shows.
- `error` is the call's error text, `""` when it succeeded. `result` is the
  output without the `Error:` line.
- A `code` rewrite from `pre-code-exec` is ignored on the engine, and the
  ledger records that fire as passed, not rewrote. Return `args` instead.
- `view_image` is the harness's own tool, not a bough call, and gets the same
  two events: `pre-code-exec` with the path as `code` (a `deny` refuses it),
  and `post-result` once the image has loaded or failed. Its `result` is not
  rewritten, since what the model reads is the image. In a project session it
  reads only what `view` may: the orb, the scratchpad and the project's own
  directory.
- The loop ignores what `stop` returns. The engine honours `block` there, so a
  stop hook written for the loop that returns `block` starts doing something
  on the engine.
- A hook that throws is reported as an error line and recorded in the ledger.
  It is never fatal, and the other hook files' results still apply, as on the
  loop.
- Hooks run on the host, even in a project session. A hook's `tools.bash` never
  runs inside the orb.
