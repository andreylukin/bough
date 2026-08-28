//! The code-mode SHELL surface, end to end on the real binary: `bash(cmd, tags)`, `sh([{cmd,
//! tags}, …])` and `bg(name, cmd)` are injected functions the sandbox can actually call, the
//! ARRAY form of the tags is what `surface/shell.md` teaches and what `bind.rs` reads, and the
//! tags land on the ledgered call.
//!
//! MERGE (`docs/codemode-merge-notes.md` §9): this is the defect the phase's own bench found and
//! could not fully close. Two halves were still open at the merge — `tools-baseline`'s `bash`
//! declared no `tags` property at all, so the surface's second argument bound to `cwd`, and no
//! row in the tree registered `sh`, so the prose taught a function nothing injected. Both are
//! fixed; these cases are what stops either from coming back.
//!
//! Nothing here is mocked. The JS runs on `js-quickjs`, the commands run in a real `sh`, and
//! every assertion reads the ledger the run left on disk.

use crate::support;

use support::codemode::{answer_round, program_round, Sandbox};

/// The one that was broken: the documented `bash(cmd, tags)` with an ARRAY of tags reaches
/// `tools-baseline`'s `bash` as a command, and the tags are on the step — not bound to `cwd`,
/// not lost, not refused by the tag rule.
#[test]
fn a_tagged_bash_from_a_program_is_one_ledgered_call_carrying_its_tags() {
    let sb = Sandbox::new("shell-bash");
    let (code, out) = sb.exec(
        "say hello",
        serde_json::json!([
            program_round(
                "c0",
                "console.log(await bash('echo hi', ['smoke', 'shell', 'echo']))"
            ),
            answer_round("said."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let calls: Vec<_> = steps
        .iter()
        .filter(|(k, b)| k == "program/call" && b["name"] == "bash")
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "the command must be exactly one ledgered call: {:?}",
        sb.kinds()
    );
    assert_eq!(
        calls[0].1["tags"],
        serde_json::json!(["smoke", "shell", "echo"]),
        "the ARRAY form's tags must be on the step: {}",
        calls[0].1
    );
    // The second positional argument is the tool's `tags` property, NOT `cwd`. That is the whole
    // bug: with no `tags` in the schema, `positional_order` put `cwd` second and the call ran in
    // a directory named after its tags — or, with `tags_required` on, never ran at all.
    assert_eq!(calls[0].1["args"]["command"], "echo hi", "{}", calls[0].1);
    assert_eq!(
        calls[0].1["args"]["tags"],
        serde_json::json!(["smoke", "shell", "echo"]),
        "the tags reach the TOOL as its declared property: {}",
        calls[0].1
    );
    assert!(
        calls[0].1["args"].get("cwd").is_none(),
        "the tags must not have bound to `cwd`: {}",
        calls[0].1
    );

    let result = steps
        .iter()
        .find(|(k, b)| k == "program/result" && b["name"] == "bash")
        .expect("the call is answered");
    assert!(
        result.1["content"]
            .as_str()
            .unwrap_or_default()
            .contains("hi"),
        "the command really ran: {}",
        result.1
    );
}

/// The other half: `sh` is a real tool now, its legs run concurrently, and a non-zero exit comes
/// back as data. Each leg's tags reach the one ledgered call, in leg order.
#[test]
fn sh_runs_its_legs_and_answers_with_objects() {
    let sb = Sandbox::new("shell-sh");
    let program = "const r = await sh([\
        {cmd: 'echo one', tags: ['echo', 'probe', 'one']},\
        {cmd: 'exit 3', tags: ['exit', 'probe', 'three']}]);\
        console.log('LEGS=' + r.length + ' OUT=' + r[0].out.trim() + ' CODE=' + r[1].code);";
    let (code, out) = sb.exec(
        "run two commands",
        serde_json::json!([program_round("c0", program), answer_round("ran.")]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let call = steps
        .iter()
        .find(|(k, b)| k == "program/call" && b["name"] == "sh")
        .unwrap_or_else(|| panic!("`sh` must be a callable function: {:?}\n{out}", sb.kinds()));
    assert_eq!(
        call.1["tags"],
        serde_json::json!(["echo", "probe", "one", "exit", "probe", "three"]),
        "a leg list carries its tags per leg, in leg order: {}",
        call.1
    );

    let run_result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    let console = run_result.1["content"].as_str().unwrap_or_default();
    assert!(
        console.contains("LEGS=2 OUT=one CODE=3"),
        "sh answers with OBJECTS and a non-zero exit is data: {console}"
    );
}

/// An untagged shell call is REFUSED and lands no call step. The rule is the point of the tags:
/// a command recorded with no tags is one no future session can find.
#[test]
fn an_untagged_bash_is_refused_and_nothing_runs() {
    let sb = Sandbox::new("shell-untagged");
    let (code, out) = sb.exec(
        "touch a file",
        serde_json::json!([
            program_round(
                "c0",
                "try { await bash('touch UNTAGGED.txt'); console.log('RAN'); } \
                 catch (e) { console.log('REFUSED=' + e.message); }"
            ),
            answer_round("refused."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let console = sb
        .steps()
        .into_iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .map(|(_, b)| b["content"].as_str().unwrap_or_default().to_string())
        .expect("the `run` call got a result");
    assert!(
        console.contains("REFUSED=") && !console.contains("RAN"),
        "an untagged command must be refused: {console}"
    );
    assert!(
        !sb.home.join("work/UNTAGGED.txt").exists(),
        "the refused command must not have run"
    );
    assert!(
        !sb.steps()
            .iter()
            .any(|(k, b)| k == "program/call" && b["name"] == "bash"),
        "a refused call lands no step: {:?}",
        sb.kinds()
    );
}

/// `bg(name, cmd)` is injected too, and its name-first signature survives the alias. The job is
/// started, so the surface's third shell verb is reachable rather than merely documented.
#[test]
fn bg_starts_a_named_job_from_a_program() {
    let sb = Sandbox::new("shell-bg");
    let (code, out) = sb.exec(
        "start something",
        serde_json::json!([
            program_round(
                "c0",
                "const j = await bg('probe job', 'echo from-bg'); console.log('JOB=' + j.name);"
            ),
            answer_round("started."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let call = steps
        .iter()
        .find(|(k, b)| k == "program/call" && b["name"] == "bg")
        .unwrap_or_else(|| panic!("`bg` must be a callable function: {:?}\n{out}", sb.kinds()));
    assert_eq!(call.1["args"]["op"], "start", "the alias fixes the op");
    assert_eq!(
        call.1["args"]["name"], "probe job",
        "the NAME comes first: {}",
        call.1
    );
    assert_eq!(call.1["args"]["cmd"], "echo from-bg", "{}", call.1);
}
