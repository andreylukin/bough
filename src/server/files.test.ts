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
Deno.test("searchWorkspaceFiles: respects .gitignore (skips ignored dirs)", async () => {
  const dir = await Deno.makeTempDir({ prefix: "gitignore-" });
  try {
    // Create a .gitignore that ignores "generated/"
    await Deno.writeTextFile(dir + "/.gitignore", "generated/\n*.log\n");
    await Deno.mkdir(dir + "/generated", { recursive: true });
    await Deno.writeTextFile(dir + "/generated/important.ts", "x");
    await Deno.writeTextFile(dir + "/real.ts", "y");
    await Deno.writeTextFile(dir + "/debug.log", "z");
    const hits = await searchWorkspaceFiles(dir, "");
    assertEquals(hits.includes("real.ts"), true);
    // Ignored dir should not appear
    assertEquals(hits.some((f) => f.includes("generated")), false);
    // Ignored file should not appear
    assertEquals(hits.some((f) => f.endsWith(".log")), false);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("searchWorkspaceFiles: directory hits ranked by basename (trailing slash)", async () => {
  const dir = await fixture();
  try {
    // "components" matches the src/components directory basename exactly.
    const hits = await searchWorkspaceFiles(dir, "components");
    assertEquals(hits.length > 0, true);
    assertEquals(hits[0], "src/components/"); // exact basename match ranks first
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
Deno.test("searchWorkspaceFiles: directory hits returned with trailing slash", async () => {
  const dir = await fixture();
  try {
    // "compon" matches the src/components directory.
    const hits = await searchWorkspaceFiles(dir, "compon");
    assertEquals(hits.includes("src/components/"), true);
    // "components" should also match.
    const hits2 = await searchWorkspaceFiles(dir, "components");
    assertEquals(hits2.includes("src/components/"), true);
    // "src" matches the src directory.
    const hits3 = await searchWorkspaceFiles(dir, "src");
    assertEquals(hits3.includes("src/"), true);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("searchWorkspaceFiles: empty query lists both files and dirs", async () => {
  const dir = await fixture();
  try {
    const all = await searchWorkspaceFiles(dir, "");
    assertEquals(all.some((f) => f.endsWith("/")), true); // at least one dir
    assertEquals(all.includes("src/"), true);
    assertEquals(all.includes("src/components/"), true);
    assertEquals(all.includes("README.md"), true); // files still present
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("expandFileReferences inlines a directory listing for @dir/ refs", async () => {
  const { expandFileReferences } = await import("./files.ts");
  const dir = await fixture();
  try {
    const out = expandFileReferences("look at @src/components/", dir);
    // Should contain a <file path="src/components/"> block with Button.tsx in it.
    if (!out.includes('<file path="src/components/">')) {
      throw new Error("expected dir listing block, got: " + out);
    }
    if (!out.includes("Button.tsx")) {
      throw new Error("expected Button.tsx in listing, got: " + out);
    }
    // Without trailing slash, the same path should still be treated as a dir
    // (statSync detects it's a directory).
    const out2 = expandFileReferences("look at @src/components", dir);
    if (!out2.includes('<file path="src/components">')) {
      throw new Error("expected dir listing for no-slash ref, got: " + out2);
    }
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("searchDirectories: fzf fragment under a base, repos ranked and marked, known seeded", async () => {
  const { searchDirectories } = await import("./files.ts");
  const root = await Deno.makeTempDir({ prefix: "dirsearch-" });
  try {
    await Deno.mkdir(`${root}/repos/bough/.git`, { recursive: true });
    await Deno.writeTextFile(`${root}/repos/bough/.git/HEAD`, "ref: refs/heads/main\n"); // a real repo has HEAD
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

// ---- image attachments ------------------------------------------------------

// A real 1×1 PNG (70 bytes) — enough to test byte-exact copy + base64 replay.
const PNG_B64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const PNG_BYTES = Uint8Array.fromBase64(PNG_B64);

Deno.test("collectImageAttachments: copies workspace image refs into destDir as image parts", async () => {
  const { collectImageAttachments } = await import("./files.ts");
  const ws = await Deno.makeTempDir({ prefix: "imgws-" });
  const dest = await Deno.makeTempDir({ prefix: "imgdest-" });
  try {
    await Deno.mkdir(`${ws}/shots`, { recursive: true });
    await Deno.writeFile(`${ws}/shots/graf.png`, PNG_BYTES);
    await Deno.writeTextFile(`${ws}/notes.txt`, "text file, not an image");

    const parts = collectImageAttachments(
      "compare @shots/graf.png with @notes.txt and @missing.png",
      ws,
      dest,
    );
    assertEquals(parts.length, 1); // .txt is not an image; missing.png doesn't exist
    const p = parts[0];
    assertEquals(p.type, "image");
    assertEquals(p.mediaType, "image/png");
    assertEquals(p.name, "shots/graf.png");
    assertEquals(p.size, PNG_BYTES.length);
    assertEquals(p.path.startsWith(`${dest}/`), true);
    assertEquals(p.path.endsWith(".png"), true);
    assertEquals(await Deno.readFile(p.path), PNG_BYTES); // byte-exact copy
  } finally {
    await Deno.remove(ws, { recursive: true });
    await Deno.remove(dest, { recursive: true });
  }
});

Deno.test("collectImageAttachments: absolute refs allowed, relative escapes/no-workspace skipped", async () => {
  const { collectImageAttachments } = await import("./files.ts");
  const dir = await Deno.makeTempDir({ prefix: "imgabs-" });
  const dest = await Deno.makeTempDir({ prefix: "imgdest-" });
  try {
    await Deno.writeFile(`${dir}/cap.jpg`, PNG_BYTES);
    // Absolute path works even with no workspace (chat-only session).
    const abs = collectImageAttachments(`see @${dir}/cap.jpg`, null, dest);
    assertEquals(abs.length, 1);
    assertEquals(abs[0].mediaType, "image/jpeg");
    // A `..` relative ref never resolves; a relative ref without a workspace is skipped.
    assertEquals(collectImageAttachments("see @../cap.jpg", dir, dest), []);
    assertEquals(collectImageAttachments("see @cap.jpg", null, dest), []);
    // Duplicated refs attach once.
    const dup = collectImageAttachments(`@${dir}/cap.jpg and @${dir}/cap.jpg`, null, dest);
    assertEquals(dup.length, 1);
  } finally {
    await Deno.remove(dir, { recursive: true });
    await Deno.remove(dest, { recursive: true });
  }
});

Deno.test("expandFileReferences leaves image refs to the image-part path", async () => {
  const { expandFileReferences } = await import("./files.ts");
  const ws = await Deno.makeTempDir({ prefix: "imgskip-" });
  try {
    await Deno.writeFile(`${ws}/shot.png`, PNG_BYTES);
    // Even though the file exists and is small, it must NOT inline as text.
    assertEquals(expandFileReferences("look at @shot.png", ws), "look at @shot.png");
  } finally {
    await Deno.remove(ws, { recursive: true });
  }
});

Deno.test("imagePartToBlock: base64 replay, missing attachment degrades to placeholder", async () => {
  const { imagePartToBlock } = await import("./files.ts");
  const dir = await Deno.makeTempDir({ prefix: "imgblk-" });
  try {
    await Deno.writeFile(`${dir}/a.png`, PNG_BYTES);
    const part = {
      type: "image" as const,
      path: `${dir}/a.png`,
      mediaType: "image/png",
      name: "shot.png",
      size: PNG_BYTES.length,
    };
    assertEquals(imagePartToBlock(part), {
      type: "image",
      data: PNG_B64,
      mediaType: "image/png",
      name: "shot.png",
    });
    // Attachment gone (e.g. wiped ~/.bough) → placeholder text, never a throw.
    assertEquals(imagePartToBlock({ ...part, path: `${dir}/gone.png` }), {
      type: "text",
      text: "[image: shot.png — attachment missing]",
    });
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
