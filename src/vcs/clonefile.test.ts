import { assert, assertEquals } from "jsr:@std/assert@1";
import { applyBack, diff, sessionDir, snapshotPaths } from "./clonefile.ts";
import type { FileDiff } from "../schema/changes.ts";

function byPath(files: FileDiff[]): Map<string, FileDiff> {
  return new Map(files.map((f) => [f.path, f]));
}

// These smokes shell out to cp (APFS clonefile) + git, so they need `--allow-run`.
// Under the current `deno task test` flags they self-skip; run them for real with
// `deno test --allow-run` (see src/sandbox/INTEGRATION.md).
async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}
const smokeOk = (await canRun("cp")) && (await canRun("git"));

Deno.test({
  name: "clonefile: snapshot → edit clones → diff → applyBack round-trip",
  ignore: !smokeOk,
  fn: async () => {
    const home = await Deno.makeTempDir({ prefix: "clone-home-" });
    const base = await Deno.makeTempDir({ prefix: "clone-snap-" });
    try {
      // Pristine originals: a file and a config dir with two files.
      const zshrc = `${home}/.zshrc`;
      const cfg = `${home}/.config/app`;
      await Deno.writeTextFile(zshrc, "export A=1\n");
      await Deno.mkdir(cfg, { recursive: true });
      await Deno.writeTextFile(`${cfg}/main.conf`, "a=1\n");
      await Deno.writeTextFile(`${cfg}/keep.conf`, "keep\n");

      // Snapshot (APFS clonefile). The agent will edit the clones under `base`.
      const map = await snapshotPaths("s1", [zshrc, cfg], base);
      assertEquals(map[zshrc], `${sessionDir("s1", base)}${zshrc}`);

      // Edit the clones: modify the file, modify one conf, add one, delete one.
      const cloneRoot = `${sessionDir("s1", base)}`;
      await Deno.writeTextFile(`${cloneRoot}${zshrc}`, "export A=1\nexport B=2\n");
      await Deno.writeTextFile(`${cloneRoot}${cfg}/main.conf`, "a=2\n");
      await Deno.writeTextFile(`${cloneRoot}${cfg}/new.conf`, "fresh\n");
      await Deno.remove(`${cloneRoot}${cfg}/keep.conf`);

      // Diff: original paths, correct statuses.
      const d = await diff("s1", base);
      assertEquals(d.source, "clonefile");
      const files = byPath(d.files);

      assertEquals(files.get(zshrc)?.status, "modified");
      assertEquals(files.get(zshrc)?.hunks[0].lines, [" export A=1", "+export B=2"]);
      assertEquals(files.get(`${cfg}/main.conf`)?.status, "modified");
      assertEquals(files.get(`${cfg}/new.conf`)?.status, "added");
      assertEquals(files.get(`${cfg}/keep.conf`)?.status, "deleted");

      // Apply back only the .zshrc edit and the keep.conf deletion; leave the rest.
      await applyBack("s1", [zshrc, `${cfg}/keep.conf`], base);

      assertEquals(await Deno.readTextFile(zshrc), "export A=1\nexport B=2\n");
      // keep.conf deletion applied → original removed.
      let keepExists = true;
      try {
        await Deno.stat(`${cfg}/keep.conf`);
      } catch {
        keepExists = false;
      }
      assert(!keepExists, "keep.conf should be removed after applyBack");

      // main.conf was NOT approved → original untouched; new.conf NOT applied.
      assertEquals(await Deno.readTextFile(`${cfg}/main.conf`), "a=1\n");
      let newExists = true;
      try {
        await Deno.stat(`${cfg}/new.conf`);
      } catch {
        newExists = false;
      }
      assert(!newExists, "unapproved new.conf must not appear in the original");
    } finally {
      await Deno.remove(home, { recursive: true });
      await Deno.remove(base, { recursive: true });
    }
  },
});

Deno.test({
  name: "clonefile: no edits yields an empty diff",
  ignore: !smokeOk,
  fn: async () => {
    const home = await Deno.makeTempDir({ prefix: "clone-home-" });
    const base = await Deno.makeTempDir({ prefix: "clone-snap-" });
    try {
      const f = `${home}/.netrc`;
      await Deno.writeTextFile(f, "machine x\n");
      await snapshotPaths("s2", [f], base);
      const d = await diff("s2", base);
      assertEquals(d.files, []);
    } finally {
      await Deno.remove(home, { recursive: true });
      await Deno.remove(base, { recursive: true });
    }
  },
});
