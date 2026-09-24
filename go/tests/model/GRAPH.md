# FizzBee state graph: where it lands and what it looks like

Everything here was observed on 2026-09-23 by running FizzBee v0.5.3
(macos_arm) and fizzbee-mbt 0.2.0 on [`example/Light.fizz`](example/Light.fizz),
a three-state cycle (red -Go-> green -Slow-> yellow -Stop-> red). Where a
statement comes from reading upstream source instead of a run, it says so.

Get the tools with `scripts/fizz.sh` (repo root). It downloads the pinned
tarballs into `${XDG_CACHE_HOME:-$HOME/.cache}/bough-fizz/0.5.3/`, checks
their sha256 against the values in the script, and execs the tool.
`scripts/fizz_test.sh` covers it offline; `FIZZ_TEST_ONLINE=1` adds the
real download and a check of `Light.fizz`.

```sh
scripts/fizz.sh fizz go/tests/model/example/Light.fizz
scripts/fizz.sh fizz --output-dir /tmp/x go/tests/model/example/Light.fizz
scripts/fizz.sh mbt-server --port 50051 --states_file <run dir>/
FIZZBEE_MBT_BIN="$(scripts/fizz.sh path mbt-runner)" go test ./...
```

## What `fizz` writes

`fizz` is a bash wrapper: `parser/parser_bin` turns `Light.fizz` into an
AST, then the Go `fizzbee` binary model-checks it.

| Path | Written when | Content |
|---|---|---|
| `<spec dir>/Light.json` | always | the parsed AST, **next to the spec** (hence `example/.gitignore`) |
| `<spec dir>/out/run_<YYYY-MM-DD_HH-MM-SS>/` | default output dir | one per run; `--test` names it `run_test` |
| `<spec dir>/out/latest` | always | symlink to the newest run dir |
| `--output-dir <dir>` | instead of `out/run_*` | same files, directly in `<dir>` |

A **passing** run dir:

```
adjacency_lists_000000_of_000000.pb   links (protobuf `Links`)
nodes_000000_of_000000.pb             states (protobuf `Nodes`)
graph.dot                             Graphviz rendering of the same graph
spec_ast.json                         copy of the AST (--copy-ast, on by default)
state_config.json                     {"options":{"maxActions":"100","maxConcurrentActions":"2","crashOnYield":true},"deadlockDetection":true}
```

A **failing** run (invariant violated) stops early and writes only the
error trace — no full `nodes_*`/`adjacency_lists_*`:

```
adjacency_lists_errors.pb   nodes_errors.pb     links/states along the failing path
error-graph.json            error-graph.dot     error-states.html
graph.dot  spec_ast.json  state_config.json
```

**`fizz` exits 0 when the model check FAILS.** Pass/fail is only on stdout:
a line starting `PASSED:` or `FAILED:` (upstream's own parallel mode greps
for `^FAILED|^DEADLOCK` for the same reason). Anything that gates on fizz
must grep, not trust `$?`.

Shard names: `nodes_%06d_of_%06d.pb` where the second number is
`len(nodes)/1_000_000` — so a small graph is `_000000_of_000000`, not
`_of_000001` (source: `modelchecker/graph.go`, `GenerateProtoOfJson`).
Links shard every 10M. Node indices are global across shards, in file order.

## The graph is protobuf, not JSON

The full state graph is **not** written as JSON. It is two protobuf
messages from upstream `proto/graph.proto` (v0.5.3):

```proto
message Nodes { repeated string json = 1; }          // one JSON string per state
message Links { int64 total_nodes = 1; repeated Link links = 2; }
message Link {
  int64 src = 1;  int64 dest = 2;                     // indices into Nodes.json
  string name = 3;                                    // action name
  repeated string labels = 4;
  double weight = 5;                                  // 1/len(outbound)
  repeated Message messages = 6;
  int64 req_id = 7;
  map<int64, int64> new_to_old_threads = 8;
  string type = 9;                                    // "action" for actions
  repeated NameValue returns = 10;                    // action return values
}
```

The node payload *is* JSON (a string inside the protobuf), so a reader needs
only the wire format, not generated code: field 1 of `Nodes` is repeated
length-delimited strings; `Links` is a varint `total_nodes` plus embedded
`Link` messages. `google.golang.org/protobuf/encoding/protowire` or ~60 lines
of `encoding/binary` decode both. Decoded from the `Light.fizz` run:

`nodes_000000_of_000000.pb` (node 0 is the initial state; later nodes have
`"roles": []` where the root has `null`):

```json
{"json": [
  {"channel_messages": {}, "channels": {}, "current": 0, "failedInvariants": null,
   "name": "yield", "returns": "{}", "roles": null,
   "state": {"color": "red"},
   "stats": {"totalActions": 0, "counts": {}},
   "threads": [], "witness": [[false]]},
  {"...": "...", "state": {"color": "green"},  "stats": {"totalActions": 1, "counts": {"Go": 1}}},
  {"...": "...", "state": {"color": "yellow"}, "stats": {"totalActions": 2, "counts": {"Go": 1, "Slow": 1}}}
]}
```

`adjacency_lists_000000_of_000000.pb`:

```json
{"total_nodes": 3, "links": [
  {"src": 0, "dest": 1, "name": "Go",   "type": "action", "weight": 1},
  {"src": 1, "dest": 2, "name": "Slow", "type": "action", "weight": 1},
  {"src": 2, "dest": 0, "name": "Stop", "type": "action", "weight": 1}
]}
```

What that means for a consumer:

- **State fields** are `node.state`, keyed by the spec's global variable
  names, values JSON-encoded from Starlark (`"red"`). Role state lives in
  `roles`, in-flight messages in `channels`/`channel_messages`.
- **Action names** are `Link.name`, exactly the spec's `action` name (roles
  would be `Role.Action`). `Init` never appears as a link: it produces
  node 0. `node.stats.counts` is the per-action tally along the BFS path
  that first reached the state.
- `node.name` is the yield point (`"yield"` for every settled state here),
  not a state identifier. States have no id field; the identity is the index.
- A node with no outbound actions gets a synthetic self-link
  `{"src": i, "dest": i, "name": "end", "type": "action"}` (source:
  `GenerateProtoOfJson`; the cyclic toy has none, so not observed).
- `failedInvariants` is `null` on good states and `{"<assertion idx>": [..]}`
  on the violating one (seen in the failing run's `nodes_errors.pb`).
- The error-path `adjacency_lists_errors.pb` links carry no `type`.

### The only JSON graph: `error-graph.json`

Written on failure only. It is the failing path, a JSON array of links, each
embedding its destination state (Go field names, capitalised), from a
variant of `Light.fizz` whose assertion was `color != "yellow"`:

```json
[
  {"Node": {"state": {"color": "red"},   "failedInvariants": null, "...": "..."},
   "Type": "", "Name": "Init", "Labels": [], "Fairness": 3, "ChoiceFairness": 0,
   "Messages": null, "ReqId": 0, "ThreadsMap": null, "FailedInvariants": null, "Returns": null},
  {"Node": {"state": {"color": "green"}, "...": "..."}, "Type": "action", "Name": "Go",   "Fairness": 1, "...": "..."},
  {"Node": {"state": {"color": "yellow"}, "failedInvariants": {"0": [0]}, "...": "..."},
   "Type": "action", "Name": "Slow", "Fairness": 1, "...": "..."}
]
```

### `graph.dot`

Same graph, node ids are Go pointers (`"0x79255ba06500"`, not stable across
runs), labels carry `State: {"color":"red"}` and edges `label="Go"`. Useful
to look at, not to parse.

## fizzbee-mbt: server and runner

Release `v0.2.0` of `github.com/fizzbee-io/fizzbee-mbt-releases` ships two
binaries per platform (`fizzbee-mbt-0.2.0-<plat>.tar.gz`), both Go 1.23.2,
both reporting build commit `5548168990633d9fffc4b7d836466aaff03c9591`,
built 2025-11-05.

`fizzbee-mbt-server` ("FizzMo"):

```
-port int            Port to run the gRPC server on (default 50051)
-states_file string  Path to the states file   <- actually the fizz run DIR
-version             Print version information
```

Given the `Light.fizz` run dir it logged `Loaded 3 nodes and 3 links`, the
state-space options from `state_config.json`, one operation group per
action (`Go`, `Slow`, `Stop`), then `FizzMo server listening on :7810`. It
binds **all interfaces** (`TCP *:7810`), not loopback.

`fizzbee-mbt-runner`:

```
-plugin-addr string       Address of the plugin service (unix socket path)
-max-actions int          (default 10)
-max-seq-runs int         (default 100)
-max-parallel-runs int
-seq-seed int  -parallel-seed int  -version
```

The runner has **no flag for the server address**: it dials
`localhost:50051` (observed: `dial tcp [::1]:50051: connect: connection
refused` with no server up). So a test run needs the server on 50051,
two MBT runs on one machine collide, and a server on any other port is
unreachable by this runner. `-plugin-addr` is always a unix socket path;
`127.0.0.1:7810` was rejected as `invalid (non-empty) authority`.

The flow, from the Go library source (`mbt/lib/go/plugin.go`): the test
calls `mbt.RunTests`, which serves the test's adapter over gRPC on a temp
unix socket and execs the runner with `--plugin-addr=<socket>` plus the
options map. The runner binary is `--fizzbee-mbt-bin` (a `go test` flag
the lib registers in `ParseFlags`), else `$FIZZBEE_MBT_BIN`, else
`fizzbee-mbt-runner` on `PATH`. The runner asks the server
(`StartTestRun`) for traces through the graph and drives the adapter
action by action. The server must be started separately beforehand.

Go library pin paired with server 0.2.0:
`github.com/fizzbee-io/fizzbee/mbt/lib/go v0.0.0-20251103175550-9e0bc037e5f4`
— the last commit under `mbt/lib/go` before the release. The lib has no
tags (pseudo-versions only). The next lib commit, `ff30d4195ab2`
(2025-12-21), adds `SentinelType sentinel_value = 6` to the `Value` oneof in
`mbt_plugin.proto`, which the 0.2.0 server predates. Its `go.mod` needs only
`google.golang.org/grpc`, `google.golang.org/protobuf`, `genproto` and
`golang.org/x/*` — pure Go, no cgo.

### License and source status

- `fizzbee-io/fizzbee` (fizz, the modelchecker, `mbt/lib/*`,
  `mbt/generator`): Apache-2.0, source public.
- `fizzbee-io/fizzbee-mbt-releases`: Apache-2.0 `LICENSE`, but the repo holds
  only `LICENSE`, `README.md` and release binaries. The server/runner
  **source is not public**: their build commit `55481689…` does not exist in
  `fizzbee-io/fizzbee` (GitHub: "No commit found for SHA"), that repo's
  `mbt/` has only `generator/` and `lib/`, and a code search there for
  `FizzMo` finds nothing. We can run and redistribute the binaries under
  Apache-2.0 but cannot build or patch them.
