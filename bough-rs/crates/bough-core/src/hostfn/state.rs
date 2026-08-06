//! `state.*` verbs (port of `src/hostfn/state.ts`): durable KV scoped to the
//! LINEAGE ROOT (fork/compaction/subagent share one store); 16KB/key refused
//! not truncated; 200-key cap (overwrite-at-cap allowed); unset get = `null`.
//! STUB (wave 2, row 2.6).
