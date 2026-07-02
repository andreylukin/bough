// normalizeWorkspace / workspaceProblem — the two pure helpers that keep a bad
// workspace path from silently killing every tool in a session.
import { assertEquals } from "jsr:@std/assert@1";
import { normalizeWorkspace, workspaceProblem } from "./workspace.ts";

Deno.test("normalizeWorkspace expands a leading ~", () => {
  const home = Deno.env.get("HOME")!;
  assertEquals(normalizeWorkspace("~/repos/app"), `${home}/repos/app`);
  assertEquals(normalizeWorkspace("~"), home);
});

Deno.test("normalizeWorkspace does not touch a real ~ mid-path or absolute paths", () => {
  assertEquals(normalizeWorkspace("/Users/x/repo"), "/Users/x/repo");
  // A tilde that isn't the home shorthand is left alone.
  assertEquals(normalizeWorkspace("/tmp/~backup"), "/tmp/~backup");
});

Deno.test("normalizeWorkspace absolutizes a relative path against cwd", () => {
  assertEquals(normalizeWorkspace("sub/dir"), `${Deno.cwd()}/sub/dir`);
});

Deno.test("workspaceProblem: ok for a real dir, message for missing/file", async () => {
  const dir = await Deno.makeTempDir();
  assertEquals(await workspaceProblem(dir), null);

  const missing = `${dir}/nope`;
  assertEquals((await workspaceProblem(missing))?.includes("does not exist"), true);

  const file = `${dir}/f.txt`;
  await Deno.writeTextFile(file, "x");
  assertEquals((await workspaceProblem(file))?.includes("not a directory"), true);

  await Deno.remove(dir, { recursive: true });
});
