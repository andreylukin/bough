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
| `{"mode":"call","tool":"ask","args":{"question":"why?"}}` | answers with one call of `tool`; its result goes out on the next request, which takes the next queued turn |
| `{"mode":"block","text":"done"}` | holds until `<name>.release` exists, then finishes with `text`; a release written by `ReleaseWith` answers as its turn says instead (`{"mode":"error"}` fails it, `{"mode":"call",…}` makes the call) |

An empty queue answers `[llm-control: no turn queued in <dir>]` rather
than waiting, so a test that queued too few turns fails instead of
hanging. Session titles and other `Complete` calls never take a turn.

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
