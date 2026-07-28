/**
 * The TUI had no command line at all: `main.tsx` never read `process.argv`, so every
 * flag was discarded in silence. These assert the two properties that matters —
 * `-w` is honoured, and anything unrecognized STOPS rather than starting anyway.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { isTuiHelpRequest, isTuiUsageError, parseTuiArgs, type TuiArgs, USAGE } from "./args.ts";

const ok = (argv: string[]): TuiArgs => {
  const parsed = parseTuiArgs(argv);
  assert.ok(!isTuiUsageError(parsed) && !isTuiHelpRequest(parsed), JSON.stringify(parsed));
  return parsed;
};

test("-w and --workspace both name where a new conversation starts", () => {
  // The bug: `bough -w /other/repo` opened in the cwd and said nothing, which
  // points an unsandboxed agent at a repository the user did not choose.
  assert.equal(ok(["-w", "/tmp/x"]).workspace, "/tmp/x");
  assert.equal(ok(["--workspace", "/tmp/x"]).workspace, "/tmp/x");
  assert.equal(ok(["--workspace=/tmp/x"]).workspace, "/tmp/x");
  assert.equal(ok(["-w=/tmp/x"]).workspace, "/tmp/x");
  // No flag at all is still the common case, and still means "the cwd".
  assert.equal(ok([]).workspace, undefined);
});

test("an unknown flag stops, rather than starting anyway", () => {
  for (const argv of [["--wrokspace", "/tmp"], ["-q"], ["--json"]]) {
    const parsed = parseTuiArgs(argv);
    assert.ok(isTuiUsageError(parsed), `${argv.join(" ")} should be refused`);
  }
  // A flag that needs a value and has none is an error, not an empty string.
  assert.ok(isTuiUsageError(parseTuiArgs(["-w"])));
  assert.ok(isTuiUsageError(parseTuiArgs(["-w", "  "])));
});

test("a positional argument is refused, and points at bough exec", () => {
  // Typing a prompt at the TUI is a real mistake, and silently swallowing it into
  // a screen that ignores it is the unhelpful answer.
  const parsed = parseTuiArgs(["fix the tests"]);
  assert.ok(isTuiUsageError(parsed));
  assert.match(parsed.usageError, /bough exec/);
});

test("--help is answered, and the usage states the posture", () => {
  for (const argv of [["--help"], ["-h"], ["-w", "/tmp", "--help"]]) {
    assert.ok(isTuiHelpRequest(parseTuiArgs(argv)), argv.join(" "));
  }
  assert.match(USAGE, /--workspace/);
  // Spec §2 — the same sentence `bough exec --help` prints.
  assert.match(USAGE, /no sandbox/);
});
