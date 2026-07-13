import { assertEquals } from "jsr:@std/assert@1";
import { parseSubagentNote } from "./lines.ts";

Deno.test("parseSubagentNote: extracts fields from the completion note", () => {
  const note = [
    '[subagent finished] "Web Frontend Analysis" (940e12a0-c614-4c4b-9fa1-dc2ec419338d) — finished, check passed.',
    "Changed files on its branch: .serena/.gitignore, .serena/project.yml.",
    "Report:",
    "# Findings\nThe web layer uses React.",
    'Its changes stay on its own branch — adopt("940e12a0-c614-4c4b-9fa1-dc2ec419338d") in run_steps merges them into this workspace; or leave the branch for review.',
  ].join("\n");
  const p = parseSubagentNote(note);
  assertEquals(p?.title, "Web Frontend Analysis");
  assertEquals(p?.sessionId, "940e12a0-c614-4c4b-9fa1-dc2ec419338d");
  assertEquals(p?.ok, true);
  assertEquals(p?.files, [".serena/.gitignore", ".serena/project.yml"]);
  assertEquals(p?.report, "# Findings\nThe web layer uses React.");
});

Deno.test("parseSubagentNote: FAILED status + no files", () => {
  const note = [
    '[subagent finished] "Broken" (abc-123) — FAILED (turn errored or was interrupted).',
    "Changed files on its branch: none.",
    "No report.",
    'Its changes stay on its own branch — adopt("abc-123") in run_steps merges them into this workspace; or leave the branch for review.',
  ].join("\n");
  const p = parseSubagentNote(note);
  assertEquals(p?.ok, false);
  assertEquals(p?.files, []);
  assertEquals(p?.report, null);
});

Deno.test("parseSubagentNote: non-note text returns null", () => {
  assertEquals(parseSubagentNote("just a normal message"), null);
});
