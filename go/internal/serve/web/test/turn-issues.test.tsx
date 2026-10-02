import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { TurnHooks, TurnView } from "../src/app";
import { groupTurns } from "../src/render";
import { turnIssues } from "../src/turn-issues";
import type { Line } from "../src/types";

const at = "2026-10-01T12:00:00Z";
const input: Line = { seq: 1, at, kind: "input", text: "Check the example." };
const failure: Line = { seq: 2, at, kind: "call", text: "grep missing example.txt", data: { id: "call-1", tool: "bash", cmd: "grep missing example.txt", exit: 1, error: "exit status 1", output: "No match" } };
const success: Line = { seq: 3, at, kind: "call", text: "cat example.txt", data: { id: "call-2", tool: "bash", cmd: "cat example.txt", exit: 0, output: "example" } };
const answer: Line = { seq: 4, at, kind: "assistant", text: "Here is the recorded result." };
const hook: Line = { seq: 5, at, kind: "hook", text: "", data: { name: "summary.js", event: "stop", error: "Error: report unavailable\n    at finish (summary.js:9:2)", input: { event: "stop" }, output: null } };
const done = (exit?: number): Line => ({ seq: 6, at, kind: "done", text: "", data: exit === undefined ? {} : { exit } });
const turn = (...body: Line[]) => groupTurns([input, ...body])[0];
const html = (...body: Line[]) => renderToStaticMarkup(<TurnView turn={turn(...body)} />);

for (const [name, body] of [
  ["a later successful call", [failure, success, answer, done(0)]],
  ["no evidence of recovery", [failure, answer, done(1)]],
] as const) {
  test(`command failure with ${name} stays local to the command`, () => {
    const result = html(...body);
    expect(turnIssues(turn(...body)).commands).toBe(1);
    expect(result).toContain('call-native block-failed');
    expect(result).toContain('class="turn-outcome">Completed');
    expect(result).toContain('class="turn-issues"');
    expect(result).toContain('1 command failed');
    expect(result).not.toContain('class="turn-outcome turn-failed"');
    expect(result).not.toContain('Done with a failed command');
    expect(result).not.toContain('Recovered');
  });
}

test("a stop hook has its own readable failure and verbatim details", () => {
  const result = html(answer, hook, done());
  expect(result).toContain('aria-label="Stop hook failed"');
  expect(result).toContain('summary.js');
  expect(result).toContain('report unavailable');
  expect(result).toContain('at finish (summary.js:9:2)');
  expect(result).toContain('class="err-raw hook-error-details"');
  expect(result).not.toContain('class="err-raw hook-error-details" open');
  expect(result).not.toContain('1 command failed');
  expect(result).not.toContain('Switch model');
  expect(result).not.toContain('>Retry<');
  expect(result).not.toContain('1 errored');
  expect(result).toContain('class="turn-outcome">Completed');
});

test("mixed hook and command failures retain their sources", () => {
  const result = html(failure, answer, hook, done(1));
  expect(result).toContain('Stop hook failed');
  expect(result).toContain('1 command failed');
  expect(result).not.toContain('2 commands failed');
  expect(result).not.toContain('Exit 1 recorded');
});

test("an untraced exit is retained without inventing its source", () => {
  const result = html(answer, hook, done(2));
  expect(result).toContain('Exit 2 recorded');
  expect(result).toContain('Exit code 2 was recorded without a matching command result.');
  expect(result).not.toContain('1 command failed');
  expect(result).toContain('Stop hook failed');
});

test("explicit provider failures and cancellation keep their own outcomes", () => {
  const fatal = html({ seq: 2, at, kind: "error", text: "401 Unauthorized" }, done());
  expect(fatal).toContain('class="err err-card"');
  expect(fatal).not.toContain('>Completed<');
  const stopped = html({ ...failure, data: { ...failure.data, canceled: true, exit: 130 } },
    { seq: 5, at, kind: "cancelled", text: "" }, done(130));
  expect(stopped).toContain('turn-outcome turn-stopped');
  expect(stopped).not.toContain('>Completed<');
  expect(stopped).not.toContain('1 command failed');
  expect(stopped).not.toContain('Exit 130 recorded');
});

test("non-shell tool failures are not called command failures", () => {
  const result = html({ ...failure, data: { id: "read", tool: "view", error: "file unavailable" } }, answer, done());
  expect(result).toContain('1 tool failed');
  expect(result).not.toContain('1 command failed');
});

test("hook error content remains escaped text and quiet hook records remain inspectable", () => {
  const result = renderToStaticMarkup(<TurnHooks lines={[
    { ...hook, data: { ...hook.data, name: '<script>bad()</script>', error: '<img src=x onerror="bad()">' } },
    { seq: 7, at, kind: "hook", text: "", data: { name: "allowed.js", event: "pre-tool-use", decision: "allowed", input: {}, output: {} } },
  ]} />);
  expect(result).toContain('&lt;script&gt;');
  expect(result).toContain('&lt;img');
  expect(result).not.toContain('<script>bad()');
  expect(result).not.toContain('<img src=x');
  expect(result).toContain('class="hook-invocation"');
  expect(result).toContain('allowed.js');
});
