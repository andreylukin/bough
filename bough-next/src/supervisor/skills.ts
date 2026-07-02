/**
 * Skills: named, reusable
 * instruction bundles the human pulls into a run by typing `/<name>` in their
 * message (e.g. `/commit tidy this up`). A skill is a folder
 * `~/.bough/skills/<name>/SKILL.md` with YAML-ish frontmatter (`name`,
 * `description`) and a markdown body of instructions. When a message names an
 * installed skill, the harness appends that skill's body to the supervisor's
 * system prompt for the run (see turn.ts).
 *
 * Override the directory with BOUGH_SKILLS_DIR (tests).
 */
import { join } from "node:path";
import { homedir } from "node:os";

export interface Skill {
  name: string;
  description: string;
}

function dir(): string {
  return Deno.env.get("BOUGH_SKILLS_DIR") ?? join(homedir(), ".bough", "skills");
}

function skillFile(name: string): string {
  return join(dir(), name, "SKILL.md");
}

/** Installed skills (name + one-line description), for discovery/UI. */
export function listSkills(): Skill[] {
  let entries: Deno.DirEntry[];
  try {
    entries = [...Deno.readDirSync(dir())];
  } catch {
    return [];
  }
  const skills: Skill[] = [];
  for (const e of entries) {
    if (!e.isDirectory) continue;
    try {
      const text = Deno.readTextFileSync(skillFile(e.name));
      skills.push({ name: e.name, description: descriptionOf(text) });
    } catch {
      // folder without a SKILL.md — not a skill
    }
  }
  return skills.sort((a, b) => a.name.localeCompare(b.name));
}

/** The markdown body of a skill (instructions), frontmatter stripped. */
export function loadBody(name: string): string | null {
  try {
    return stripFrontmatter(Deno.readTextFileSync(skillFile(name)));
  } catch {
    return null;
  }
}

/**
 * The supervisor-prompt section for every installed skill the message invokes via
 * `/<name>`. Empty when none are named (or installed).
 */
export function activeFor(message: string): string {
  const sections: string[] = [];
  for (const skill of listSkills()) {
    if (!mentions(message, skill.name)) continue;
    const body = loadBody(skill.name);
    if (body === null) continue;
    sections.push(
      `\n\n# Active skill: /${skill.name}\n` +
        `The human invoked the \`/${skill.name}\` skill for this task. Follow its ` +
        "instructions, which are authoritative for how to do the work (but never " +
        "override the safety and sandbox rules above):\n\n" +
        body.trim(),
    );
  }
  return sections.join("");
}

/** True when `message` contains the token `/<name>` at a word boundary. */
function mentions(message: string, name: string): boolean {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|\\s)/${escaped}(\\s|$)`).test(message);
}

/** `description:` from the frontmatter, or "" if absent. */
function descriptionOf(text: string): string {
  for (const line of frontmatterLines(text)) {
    const idx = line.indexOf(":");
    if (idx > 0 && line.slice(0, idx).trim() === "description") {
      return line.slice(idx + 1).trim();
    }
  }
  return "";
}

function frontmatterLines(text: string): string[] {
  if (!text.trimStart().startsWith("---")) return [];
  const parts = text.split("---");
  return parts.length >= 2 ? parts[1].split("\n") : [];
}

function stripFrontmatter(text: string): string {
  if (!text.trimStart().startsWith("---")) return text;
  const parts = text.split("---");
  return parts.length >= 3 ? parts.slice(2).join("---").trimStart() : text;
}
