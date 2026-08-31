//! `spawn_worker` executes — under BOTH consumers.
//!
//! MERGE (`docs/codemode-merge-notes.md` §7, and track C's H-C5, found independently): a TOOL
//! executes under the context of whoever executed it, and that is the agent loop. `tool-workers`
//! resolves the `workers` seam from `ToolCx.ctx`, so the read was attributed to `agent.loop`,
//! whose `inject()` did not name the key, and the declared-key rule refused it:
//!
//! ```text
//! workers seam unavailable: plugin `agent-loop` (row `agent.loop`) read service `workers`
//! without declaring it in inject
//! ```
//!
//! No agent could spawn a worker through the tool surface AT ALL — typed or code mode, on the
//! shipped tree. Both branches recorded it and neither could fix it (`plugins/agent-loop` was the
//! other track's). The fix is one optional key; these cases are what keeps it.

use crate::support;

use support::codemode::{answer_round, program_round, tool_round, Sandbox};

/// The task the worker is given, and the answer the recorded transcript gives back for it. The
/// SECOND round is the worker's own: a worker runs the same loop against the same replay row.
const TASK: &str = "Read README.md and say what its second line is.";
const WORKER_ANSWER: &str = "The second line of README.md is `two`.";

fn rounds(first: serde_json::Value) -> serde_json::Value {
    serde_json::json!([
        first,
        answer_round(WORKER_ANSWER),
        answer_round("Spawned a worker; it read the file."),
    ])
}

/// What the failure looked like, spelled once. A result carrying this is the regression.
const REFUSAL: &str = "without declaring it in inject";

fn assert_spawned(sb: &Sandbox, out: &str) {
    let steps = sb.steps();
    let kinds = sb.kinds();
    let results: Vec<_> = steps
        .iter()
        .filter(|(k, b)| {
            (k == "tool/result" || k == "program/result") && b["name"] == "spawn_worker"
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "the spawn must be exactly one answered call: {kinds:?}\n{out}"
    );
    let body = results[0].1.to_string();
    assert!(
        !body.contains(REFUSAL),
        "the `workers` seam is unreachable from the loop again: {body}"
    );
    assert!(
        kinds.iter().any(|k| k == "worker/started"),
        "a worker really started: {kinds:?}\n{out}"
    );
}

/// The typed surface: the model calls `spawn_worker` by name.
#[test]
fn a_worker_spawns_through_the_typed_tool_surface() {
    let sb = Sandbox::typed("workers-typed");
    let (code, out) = sb.exec(
        "delegate the read",
        rounds(tool_round(
            "c0",
            "spawn_worker",
            serde_json::json!({ "task": TASK }),
        )),
    );
    assert_eq!(code, 0, "{out}");
    assert_spawned(&sb, &out);
}

/// Code mode: the same seam, reached as the injected `agent()` the bundle aliases to it.
#[test]
fn a_worker_spawns_from_inside_a_program() {
    let sb = Sandbox::new("workers-codemode");
    let program = format!("console.log('SPAWNED=' + JSON.stringify(await agent({TASK:?})))");
    let (code, out) = sb.exec("delegate the read", rounds(program_round("c0", &program)));
    assert_eq!(code, 0, "{out}");
    assert_spawned(&sb, &out);
}

/// The rule the fix rests on, said once so a new tool cannot quietly break it: any tool in the
/// tree that resolves a seam from `ToolCx.ctx` must name a key the LOOP declares, because the
/// loop is what executes it.
///
/// A source grep, in the spirit of `tools-codemode`'s roster test: it reads the tree rather than a
/// list somebody has to remember to update.
#[test]
fn every_seam_a_tool_reads_through_its_call_context_is_declared_by_the_loop() {
    let root = support::repo_root();
    let declared = std::fs::read_to_string(root.join("plugins/agent-loop/src/lib.rs"))
        .expect("the loop's source");
    let inject = declared
        .split("fn inject()")
        .nth(1)
        .and_then(|s| s.split("\n    }").next())
        .expect("agent-loop declares an inject")
        .to_string();

    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![root.join("plugins")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                if path.file_name().and_then(|n| n.to_str()) != Some("target") {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().and_then(|x| x.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            // `cx.ctx.get::<Seam>()` inside a tool body — the exact shape that made
            // `spawn_worker` dead. The KEY is the seam's lowercase type name, which is the
            // convention every `Inject` list in the tree already uses.
            for frag in text.split("cx.ctx.get::<").skip(1) {
                let ty = frag.split('>').next().unwrap_or_default();
                let key = ty.rsplit("::").next().unwrap_or(ty).to_lowercase();
                if key.is_empty() || inject.contains(&format!("\"{key}\"")) {
                    continue;
                }
                offenders.push(format!("{}: reads `{key}`", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a tool reaches a seam the agent loop does not declare, so the call will be refused at \
         runtime — add the key to `agent-loop::inject` as an optional one:\n{}",
        offenders.join("\n")
    );
}
