# History

Finished documents, kept for their reasoning and **not maintained**. Nothing here
describes the system as it is today, and nothing here is a to-do list.

| | |
|---|---|
| [implementation-plan.md](implementation-plan.md) | The build order for the Deno/Ink implementation. Two rewrites old — the module layout it describes is gone. Worth reading only for why things were sequenced the way they were. |
| [port-plan.md](port-plan.md) | The TypeScript-to-Rust port, row by row. Complete. Module comments across `crates/` still cite its rows (`PORT_PLAN row 2.21`) as a map from a contract to where it landed, which is why it is kept rather than deleted. |

For what bough is now: [`../spec.md`](../spec.md) is authoritative, [`../../specs/`](../../specs)
pins the per-subsystem contracts, and [`../README.md`](../README.md) is the map.
