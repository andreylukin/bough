import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { activeFor, activeSkills, listSkills, loadBody } from "./skills.ts";

function withSkillsDir(fn: (dir: string) => void) {
  const dir = Deno.makeTempDirSync({ prefix: "bough-skills-" });
  Deno.env.set("BOUGH_SKILLS_DIR", dir);
  // Pin the bundled dir away from the real repo skills/ so tests stay hermetic.
  Deno.env.set("BOUGH_BUNDLED_SKILLS_DIR", "/nonexistent-bough-bundled");
  try {
    fn(dir);
  } finally {
    Deno.env.delete("BOUGH_SKILLS_DIR");
    Deno.env.delete("BOUGH_BUNDLED_SKILLS_DIR");
  }
}

function install(dir: string, name: string, description: string, body: string) {
  Deno.mkdirSync(`${dir}/${name}`, { recursive: true });
  Deno.writeTextFileSync(
    `${dir}/${name}/SKILL.md`,
    `---\nname: ${name}\ndescription: ${description}\n---\n\n${body}\n`,
  );
}

Deno.test("listSkills reads frontmatter descriptions; loadBody strips frontmatter", () => {
  withSkillsDir((dir) => {
    install(dir, "commit", "make a tidy commit", "Stage and commit with a conventional message.");
    install(dir, "review", "review the diff", "Read the diff and comment.");
    // Installed skills, minus the always-present builtins.
    const installed = listSkills().filter((s) => !["init", "mcp"].includes(s.name));
    assertEquals(installed, [
      { name: "commit", description: "make a tidy commit" },
      { name: "review", description: "review the diff" },
    ]);
    assertEquals(loadBody("commit"), "Stage and commit with a conventional message.\n");
  });
});

Deno.test("the builtins are available without an install", () => {
  Deno.env.set("BOUGH_SKILLS_DIR", "/nonexistent-bough-skills");
  Deno.env.set("BOUGH_BUNDLED_SKILLS_DIR", "/nonexistent-bough-bundled");
  try {
    assertEquals(listSkills().map((s) => s.name), ["init", "mcp"]);
    assertStringIncludes(activeFor("/init"), "AGENTS.md");
  } finally {
    Deno.env.delete("BOUGH_SKILLS_DIR");
  }
});

Deno.test("activeFor injects only /named skills at word boundaries", () => {
  withSkillsDir((dir) => {
    install(dir, "commit", "d", "COMMIT INSTRUCTIONS");
    install(dir, "review", "d", "REVIEW INSTRUCTIONS");
    const section = activeFor("/commit tidy up the worktree");
    assertStringIncludes(section, "Active skill: /commit");
    assertStringIncludes(section, "COMMIT INSTRUCTIONS");
    assertEquals(section.includes("REVIEW"), false);
    // No bare-word or substring triggering.
    assertEquals(activeFor("please commit this"), "");
    assertEquals(activeFor("see /commitment docs"), "");
  });
});

Deno.test("no skills dir → only builtins remain; unknown /names inject nothing", () => {
  Deno.env.set("BOUGH_SKILLS_DIR", "/nonexistent-bough-skills");
  Deno.env.set("BOUGH_BUNDLED_SKILLS_DIR", "/nonexistent-bough-bundled");
  try {
    assertEquals(listSkills().map((s) => s.name), ["init", "mcp"]);
    assertEquals(activeFor("/anything at all"), "");
  } finally {
    Deno.env.delete("BOUGH_SKILLS_DIR");
    Deno.env.delete("BOUGH_BUNDLED_SKILLS_DIR");
  }
});

Deno.test("bundled skills: listed, ${SKILL_DIR} resolves, installed name loses to bundled", () => {
  const bundled = Deno.makeTempDirSync({ prefix: "bough-bundled-" });
  const installed = Deno.makeTempDirSync({ prefix: "bough-skills-" });
  Deno.env.set("BOUGH_BUNDLED_SKILLS_DIR", bundled);
  Deno.env.set("BOUGH_SKILLS_DIR", installed);
  try {
    Deno.mkdirSync(`${bundled}/history`);
    Deno.writeTextFileSync(
      `${bundled}/history/SKILL.md`,
      "---\nname: history\ndescription: bundled history\n---\n\nrun ${SKILL_DIR}/helper.py\n",
    );
    Deno.mkdirSync(`${installed}/history`);
    Deno.writeTextFileSync(
      `${installed}/history/SKILL.md`,
      "---\nname: history\ndescription: installed shadow\n---\n\nSHADOWED\n",
    );
    const history = listSkills().find((s) => s.name === "history");
    assertEquals(history?.description, "bundled history");
    // One entry despite two sources, body from the bundled file, dir substituted.
    assertEquals(listSkills().filter((s) => s.name === "history").length, 1);
    assertEquals(loadBody("history"), `run ${bundled}/history/helper.py\n`);
  } finally {
    Deno.env.delete("BOUGH_BUNDLED_SKILLS_DIR");
    Deno.env.delete("BOUGH_SKILLS_DIR");
  }
});

Deno.test("mcp frontmatter: parsed as a list; activeSkills unions invoked skills' servers", () => {
  withSkillsDir((dir) => {
    Deno.mkdirSync(`${dir}/browse`, { recursive: true });
    Deno.writeTextFileSync(
      `${dir}/browse/SKILL.md`,
      "---\nname: browse\ndescription: drive chrome\nmcp: chrome-devtools, linear\n---\n\nDRIVE\n",
    );
    install(dir, "plain", "no servers", "PLAIN");

    const browse = listSkills().find((s) => s.name === "browse");
    assertEquals(browse?.mcp, ["chrome-devtools", "linear"]);
    assertEquals(listSkills().find((s) => s.name === "plain")?.mcp, undefined);

    const active = activeSkills("/browse the dashboard");
    assertEquals(active.servers, ["chrome-devtools", "linear"]);
    assertStringIncludes(active.sections, "DRIVE");
    // a skill without mcp grants nothing; no invocation grants nothing
    assertEquals(activeSkills("/plain task").servers, []);
    assertEquals(activeSkills("no skills here").servers, []);
  });
});

Deno.test("the /mcp builtin manages servers via the loopback API", () => {
  const body = loadBody("mcp") ?? "";
  assertStringIncludes(body, "/mcp/servers");
  assertStringIncludes(body, "restart");
  assertStringIncludes(body, "enable");
  assertStringIncludes(body, "$BOUGH_SESSION");
});
