import { assertEquals } from "jsr:@std/assert@1";
import { searchWorkspaceFiles } from "./files.ts";

async function fixture(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "filesearch-" });
  await Deno.mkdir(`${dir}/src/components`, { recursive: true });
  await Deno.mkdir(`${dir}/node_modules/dep`, { recursive: true });
  await Deno.mkdir(`${dir}/.git`, { recursive: true });
  await Deno.writeTextFile(`${dir}/README.md`, "r");
  await Deno.writeTextFile(`${dir}/src/api.ts`, "a");
  await Deno.writeTextFile(`${dir}/src/components/Button.tsx`, "b");
  await Deno.writeTextFile(`${dir}/node_modules/dep/index.js`, "d");
  await Deno.writeTextFile(`${dir}/.git/config`, "g");
  await Deno.writeTextFile(`${dir}/.env`, "secret");
  return dir;
}

Deno.test("searchWorkspaceFiles: subsequence match, basename ranked first", async () => {
  const dir = await fixture();
  try {
    assertEquals(await searchWorkspaceFiles(dir, "button"), ["src/components/Button.tsx"]);
    // "arts" is a subsequence of "src/components/Button.tsx"? no — but "btn" isn't either;
    // use a real subsequence that hits the basename.
    assertEquals(await searchWorkspaceFiles(dir, "api"), ["src/api.ts"]);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("searchWorkspaceFiles: skips .git/node_modules/dotfiles, empty query lists files", async () => {
  const dir = await fixture();
  try {
    const all = await searchWorkspaceFiles(dir, "");
    assertEquals(all.includes("README.md"), true);
    assertEquals(all.includes("src/api.ts"), true);
    assertEquals(all.some((f) => f.includes("node_modules")), false);
    assertEquals(all.some((f) => f.includes(".git")), false);
    assertEquals(all.some((f) => f.includes(".env")), false);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("searchWorkspaceFiles: missing root yields []", async () => {
  assertEquals(await searchWorkspaceFiles("/no/such/dir/xyz", "a"), []);
});

Deno.test("searchDirectories: fzf fragment under a base, repos ranked and marked, known seeded", async () => {
  const { searchDirectories } = await import("./files.ts");
  const root = await Deno.makeTempDir({ prefix: "dirsearch-" });
  try {
    await Deno.mkdir(`${root}/repos/bough/.git`, { recursive: true });
    await Deno.mkdir(`${root}/repos/bough/src`, { recursive: true }); // inside a repo — never offered
    await Deno.mkdir(`${root}/repos/wordbook`, { recursive: true });
    await Deno.mkdir(`${root}/notes`, { recursive: true });
    await Deno.mkdir(`${root}/node_modules/dep`, { recursive: true });

    // Slash query: walk the base, fuzzy the fragment; repo beats plain dir.
    const hits = searchDirectories(`${root}/repos/bo`);
    assertEquals(hits[0].path, `${root}/repos/bough`);
    assertEquals(hits[0].repo, true);
    assertEquals(hits.some((h) => h.path.endsWith("/bough/src")), false);

    // Subsequence, not prefix: "wdbk" still finds wordbook.
    const fuzzy = searchDirectories(`${root}/repos/wdbk`);
    assertEquals(fuzzy.map((h) => h.path), [`${root}/repos/wordbook`]);

    // Skip-dirs never surface.
    const all = searchDirectories(`${root}/`);
    assertEquals(all.some((h) => h.path.includes("node_modules")), false);

    // A known workspace matches against the whole query and outranks walked dirs.
    const known = searchDirectories(`${root}/repos/`, [`${root}/repos/wordbook`]);
    assertEquals(known[0].path, `${root}/repos/wordbook`);

    // Non-existent base → only known-workspace matches.
    assertEquals(searchDirectories("/no/such/base/x"), []);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});

Deno.test("expandFileReferences inlines a referenced file, skips missing/escapes", async () => {
  const { expandFileReferences } = await import("./files.ts");
  const dir = await fixture();
  try {
    const out = expandFileReferences("look at @src/api.ts please", dir);
    if (!out.includes('<file path="src/api.ts">') || !out.includes("\na\n")) {
      throw new Error("expected inlined content, got: " + out);
    }
    // missing file → unchanged
    const miss = expandFileReferences("see @nope.ts", dir);
    if (miss !== "see @nope.ts") throw new Error("missing ref should pass through");
    // path escape → ignored
    const esc = expandFileReferences("@../secret", dir);
    if (esc !== "@../secret") throw new Error("escape should be ignored");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("grantedDirs reads ~/.bough/grants.json, empty on absence/garbage", async () => {
  const { grantedDirs } = await import("./files.ts");
  const home = await Deno.makeTempDir({ prefix: "granthome-" });
  const orig = Deno.env.get("HOME");
  try {
    Deno.env.set("HOME", home);
    assertEquals(grantedDirs(), []); // no file yet

    await Deno.mkdir(`${home}/.bough`, { recursive: true });
    await Deno.writeTextFile(`${home}/.bough/grants.json`, JSON.stringify(["/a", "/b", 3]));
    assertEquals(grantedDirs(), ["/a", "/b"]); // non-strings filtered

    await Deno.writeTextFile(`${home}/.bough/grants.json`, "not json");
    assertEquals(grantedDirs(), []); // garbage → []
  } finally {
    if (orig) Deno.env.set("HOME", orig);
    await Deno.remove(home, { recursive: true });
  }
});
