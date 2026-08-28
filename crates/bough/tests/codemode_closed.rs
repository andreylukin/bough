//! V3 — the sandbox is CLOSED, the caps TERMINATE, and `bash` is the only way to run a command.
//!
//! The engine-level half of V3 lives next to the engine
//! (`plugins/js-quickjs/src/engine.rs`: the global surface, the ambient world, the three caps).
//! What can only be decided out here, on the REAL binary with the shipped code-mode rows, is the
//! composition: that a cap breach becomes a typed `program/error` STEP, and that a command the
//! program ran is a ledgered `program/call` carrying its tags and its real output.
//!
//! Nothing here is mocked. The JS is executed by `js-quickjs`, the shell is `tools-baseline`'s
//! real `bash`, and every assertion reads the ledger the run left on disk.

mod support;

use support::codemode::{answer_round, program_round, Sandbox};

/// Rewrite the caps in the sandbox's own copy of the code-mode bundle. The caps are a
/// deployment-varying config field (§0.2), so a test bends them the way a deployment would.
fn set_caps(sb: &Sandbox, ops: u64, wall_ms: u64) {
    let path = sb.root().join("bundles/bough-codemode.yml");
    let text = std::fs::read_to_string(&path).unwrap();
    let text = text
        .replace("ops: 20000000", &format!("ops: {ops}"))
        .replace("wall_ms: 120000", &format!("wall_ms: {wall_ms}"));
    std::fs::write(&path, text).unwrap();
}

/// The closure proof at the composed level: a program running under the shipped rows sees no
/// file, network, environment or process capability — and the one thing it CAN reach, `view`,
/// is an injected function whose result comes back through the pipeline.
#[test]
fn no_file_network_env_or_process_access_exists_except_through_injected_functions() {
    let sb = Sandbox::new("closed");
    std::fs::write(sb.home.join("work/secret.txt"), "PASSWORD=hunter2\n").unwrap();
    let probe = "const names = ['fetch','XMLHttpRequest','WebSocket','require','module',\
                 'process','Deno','Bun','Buffer','std','os','scriptArgs','print','open',\
                 'readFile','writeFile','setTimeout','Worker','navigator','localStorage'];\
                 console.log('LEAKED=' + (names.filter(n => globalThis[n] !== undefined).join(',') || 'none'));\
                 try { await import('fs'); console.log('IMPORTED'); } catch (e) { console.log('import-refused'); }\
                 console.log('VIEW=' + (await view('README.md')).includes('two'));";
    let (code, out) = sb.exec(
        "probe the sandbox",
        serde_json::json!([program_round("c0", probe), answer_round("closed.")]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    let console = result.1["content"].as_str().unwrap_or_default().to_string();
    assert!(
        console.contains("LEAKED=none"),
        "a capability leaked into the sandbox: {console}"
    );
    assert!(
        console.contains("import-refused") && !console.contains("IMPORTED"),
        "a module loader is reachable: {console}"
    );
    assert!(
        console.contains("VIEW=true"),
        "the injected function is the path that DOES work: {console}"
    );
    // The program never touched the secret, and nothing in the ledger did either.
    assert!(
        !steps.iter().any(|(_, b)| b.to_string().contains("hunter2")),
        "the sandbox reached a file no injected function was asked for"
    );
}

/// A runaway loop is terminated by the OPS cap, and the termination is a typed step, not a hang.
#[test]
fn a_program_past_the_ops_cap_is_terminated_and_lands_a_typed_program_error_step() {
    let sb = Sandbox::new("ops");
    set_caps(&sb, 50_000, 120_000);
    let started = std::time::Instant::now();
    let (code, out) = sb.exec(
        "loop forever",
        serde_json::json!([
            program_round("c0", "while (true) {}"),
            answer_round("stopped."),
        ]),
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(90),
        "the ops cap did not bite: {:?}",
        started.elapsed()
    );

    let errors: Vec<_> = sb
        .steps()
        .into_iter()
        .filter(|(k, _)| k == "program/error")
        .collect();
    assert_eq!(errors.len(), 1, "one terminal error step: {errors:?}");
    assert_eq!(
        errors[0].1["error"]["kind"], "ops_exceeded",
        "the error is TYPED, not a message: {}",
        errors[0].1
    );
    assert_eq!(errors[0].1["error"]["ops"], 50_000);
}

/// The same, for the wall clock: a program parked past `wall_ms` is cut off with `time_exceeded`.
#[test]
fn a_program_past_the_time_cap_is_terminated_and_lands_a_typed_program_error_step() {
    let sb = Sandbox::new("time");
    set_caps(&sb, 100_000_000_000, 1_500);
    let started = std::time::Instant::now();
    let (code, out) = sb.exec(
        "loop forever",
        serde_json::json!([
            program_round("c0", "while (true) {}"),
            answer_round("stopped."),
        ]),
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(90),
        "the time cap did not bite: {:?}",
        started.elapsed()
    );

    let errors: Vec<_> = sb
        .steps()
        .into_iter()
        .filter(|(k, _)| k == "program/error")
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].1["error"]["kind"], "time_exceeded",
        "{}",
        errors[0].1
    );
    assert_eq!(errors[0].1["error"]["ms"], 1_500);
}

/// `bash` is the command path, and taking it is LEDGERED: a `program/call` step naming `bash`,
/// carrying the tags the program passed and the command it ran, plus a `program/result` with the
/// command's real output. Model-visible ⟺ ledgered, for a shell command.
#[test]
fn every_command_is_a_ledgered_program_call_with_its_tags() {
    let sb = Sandbox::new("bash");
    let (code, out) = sb.exec(
        "run a command",
        serde_json::json!([
            program_round(
                "c0",
                "console.log(await bash('echo hello-from-the-sandbox', 'shell:probe:demo'))"
            ),
            answer_round("ran."),
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
        serde_json::json!(["shell", "probe", "demo"]),
        "the tags are on the step: {}",
        calls[0].1
    );
    assert_eq!(
        calls[0].1["args"]["command"], "echo hello-from-the-sandbox",
        "{}",
        calls[0].1
    );

    let results: Vec<_> = steps
        .iter()
        .filter(|(k, b)| k == "program/result" && b["name"] == "bash")
        .collect();
    assert_eq!(results.len(), 1, "every call is answered");
    assert!(
        results[0].1["content"]
            .as_str()
            .unwrap_or_default()
            .contains("hello-from-the-sandbox"),
        "the command really ran: {}",
        results[0].1
    );
    // And what the model got back is the console, which carries the same output.
    let run_result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    assert!(
        run_result.1["content"]
            .as_str()
            .unwrap_or_default()
            .contains("hello-from-the-sandbox"),
        "{}",
        run_result.1["content"]
    );
}
