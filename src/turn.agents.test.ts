import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { readAgentsFile } from "./turn.ts";

function tmp(): string {
  return Deno.makeTempDirSync({ prefix: "bough-agents-" });
}

Deno.test("readAgentsFile: null when neither global nor workspace file exists", async () => {
  const ws = tmp();
  Deno.env.set("BOUGH_GLOBAL_AGENTS", tmp() + "/none.md");
  try {
    assertEquals(await readAgentsFile(ws), null);
  } finally {
    Deno.env.delete("BOUGH_GLOBAL_AGENTS");
  }
});

Deno.test("readAgentsFile: reads the workspace AGENTS.md", async () => {
  const ws = tmp();
  Deno.writeTextFileSync(ws + "/AGENTS.md", "# WS\nTOKEN-PROJECT-777\n");
  Deno.env.set("BOUGH_GLOBAL_AGENTS", tmp() + "/none.md");
  try {
    const out = await readAgentsFile(ws);
    assertStringIncludes(out!, "# Project rules (AGENTS.md)");
    assertStringIncludes(out!, "Workspace rules (AGENTS.md)");
    assertStringIncludes(out!, "TOKEN-PROJECT-777");
  } finally {
    Deno.env.delete("BOUGH_GLOBAL_AGENTS");
  }
});

Deno.test("readAgentsFile: reads the global ~/.bough/AGENTS.md", async () => {
  const ws = tmp();
  const g = tmp() + "/global.md";
  Deno.writeTextFileSync(g, "# Global\nTOKEN-GLOBAL-555\n");
  Deno.env.set("BOUGH_GLOBAL_AGENTS", g);
  try {
    const out = await readAgentsFile(ws);
    assertStringIncludes(out!, "Global rules (~/.bough/AGENTS.md)");
    assertStringIncludes(out!, "TOKEN-GLOBAL-555");
  } finally {
    Deno.env.delete("BOUGH_GLOBAL_AGENTS");
  }
});

Deno.test("readAgentsFile: includes BOTH files, global before workspace", async () => {
  const ws = tmp();
  const g = tmp() + "/global.md";
  Deno.writeTextFileSync(g, "GLOBAL-CANARY-OTTER\n");
  Deno.writeTextFileSync(ws + "/AGENTS.md", "PROJECT-CANARY-FALCON\n");
  Deno.env.set("BOUGH_GLOBAL_AGENTS", g);
  try {
    const out = await readAgentsFile(ws)!;
    assertStringIncludes(out!, "GLOBAL-CANARY-OTTER");
    assertStringIncludes(out!, "PROJECT-CANARY-FALCON");
    // global section comes first, workspace second
    const gi = out!.indexOf("GLOBAL-CANARY-OTTER");
    const pi = out!.indexOf("PROJECT-CANARY-FALCON");
    assertEquals(gi < pi, true);
  } finally {
    Deno.env.delete("BOUGH_GLOBAL_AGENTS");
  }
});

Deno.test("readAgentsFile: empty/whitespace files are ignored", async () => {
  const ws = tmp();
  const g = tmp() + "/global.md";
  Deno.writeTextFileSync(g, "   \n  \n");
  Deno.writeTextFileSync(ws + "/AGENTS.md", "");
  Deno.env.set("BOUGH_GLOBAL_AGENTS", g);
  try {
    assertEquals(await readAgentsFile(ws), null);
  } finally {
    Deno.env.delete("BOUGH_GLOBAL_AGENTS");
  }
});
