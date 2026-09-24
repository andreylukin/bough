# llm-control: a model a test steers turn by turn

`llm-control` is an llm row for tests. It is linked into the binary
like `llm-echo` and `llm-script` but is not in the default `bough.yml`;
a test selects it with `--set llm.plugin=llm-control`. It drives the
default engine (`engine-unreal`) only.

Each model request takes the lexically first `<name>.json` in the
control dir (default `$HOME/.bough/llm-control`, or `--set
llm.dir=<path>`), renames it `<name>.taken`, and answers as it says:

| file | the request |
|---|---|
| `{"mode":"ok","text":"done"}` | finishes with `text` |
| `{"mode":"error","error":"boom"}` | fails; the turn errors and headless exits 1 |
| `{"mode":"slow","text":"a b c","delay_ms":300}` | streams `text` a word per `delay_ms` |
| `{"mode":"ok","calls":[{"name":"bash","args":{"command":"…"}}]}` | makes those tool calls; the engine runs them and its next request takes the next queued turn |
| `{"mode":"call","tool":"ask","args":{"question":"why?"}}` | answers with one call of `tool`; its result goes out on the next request, which takes the next queued turn |
| `{"mode":"block","text":"done"}` | holds until `<name>.release` exists, then finishes with `text`; a release written by `ReleaseWith` answers as its turn says instead (`{"mode":"error"}` fails it, `{"call":{"name":"ask","args":{...}}}` answers with that tool call after its text, `{"mode":"call",…}` makes the call, `{"bash":"cmd"}` answers with one bash tool call running `cmd`, after which the engine asks again and takes the next queued turn) |

An `{"mode":"api"}` turn goes through the real Messages API adapter
(`internal/messagesapi`) and its retry loop, with a budget of two
attempts, against an HTTP server the row plays in process: nothing
leaves the process. Each HTTP attempt writes `<name>.attempt-<k>` and
is held; `<name>.stream` streams a fragment into it (`Stream`), and
`<name>.answer` ends it (`AnswerWith`): `ok` (text, then `calls`),
`transient` (a 529, or an in-stream `overloaded_error` after output:
retried), `fatal` (a 400: not retried), `overflow` (a 400 "prompt is too
long": a context overflow), `refused` or `max_tokens`. Before a retry the
adapter waits, writing `<name>.waiting`, until `<name>.retry` exists
(`WaitRetryWait`, `Retry`), so a test can stand in the wait. `answer` on
the queued turn answers its first attempt at once. `model` on the row
(`/model <id>`) names the model the row reports, `control` by default.

While a `block` turn is held, each `<name>.say-<n>` the test writes
(`control.Say`) is streamed as one live assistant delta and renamed
`<name>.said-<n>`: text the session shows and never records.

With `hold_boot: true` on the row, a fresh session (one with
`BOUGH_SESSION_ID` and no history file yet) stops before it writes its
history file, writes `boot/<id>.waiting` (content `main` for a
project's main thread, else `session`) and waits for
`boot/<id>.release` (`Booting`, `WaitBooting`, `ReleaseBoot`). serve's
Create waits for that file, so a test can hold a session in
"starting"; a restart or reload of a session that has a file is not
held.

An empty queue answers `[llm-control: no turn queued in <dir>]` rather
than waiting, so a test that queued too few turns fails instead of
hanging. Session titles and other `Complete` calls never take a turn.

A process can also be held **before its history file exists**: while
`start.hold` is in the control dir (`HoldStart`), every process that
mounts the row parks there (the llm row mounts before history) and
announces itself as `start-<pid>.held` (`Held`, `WaitHeld`).
`ReleaseStart` lets it go on; `ExitStart` makes it exit 3 instead. This
is how the session-create flow puts serve's Create between spawn and
history, or makes its child die there.

The row lives in `plugins/llm/control.go` (only that package may import
the harness); this package is the test side:

```go
import control "github.com/andreylukin/bough/tests/model/llm"

dir := control.Dir(home) // the child's HOME
control.Queue(t, dir, "001", control.Turn{Mode: "block", Text: "released"})
// ... run `bough --headless --set llm.plugin=llm-control`, send a line ...
control.WaitTaken(t, dir, "001", 30*time.Second) // the request is in flight
control.Release(t, dir, "001")
```

The tests here build the binary in `TestMain`, so `go test` cannot see
a change to the row through its cache; run them with `-count=1`:

```sh
go test -count=1 -race -parallel 4 ./tests/model/llm/
```
