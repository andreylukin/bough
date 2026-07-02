import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { activeFor, listSkills, loadBody } from "./skills.ts";

function withSkillsDir(fn: (dir: string) => void) {
  const dir = Deno.makeTempDirSync({ prefix: "bough-skills-" });
  Deno.env.set("BOUGH_SKILLS_DIR", dir);
  try {
    fn(dir);
  } finally {
    Deno.env.delete("BOUGH_SKILLS_DIR");
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
    // Installed skills, minus the always-present builtins (e.g. /init).
    const installed = listSkills().filter((s) => s.name !== "init");
    assertEquals(installed, [
      { name: "commit", description: "make a tidy commit" },
      { name: "review", description: "review the diff" },
    ]);
    assertEquals(loadBody("commit"), "Stage and commit with a conventional message.\n");
  });
});

Deno.test("the /init builtin is available without an install", () => {
  Deno.env.set("BOUGH_SKILLS_DIR", "/nonexistent-bough-skills");
  try {
    assertEquals(listSkills().map((s) => s.name), ["init"]);
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
  try {
    assertEquals(listSkills().map((s) => s.name), ["init"]);
    assertEquals(activeFor("/anything at all"), "");
  } finally {
    Deno.env.delete("BOUGH_SKILLS_DIR");
  }
});
