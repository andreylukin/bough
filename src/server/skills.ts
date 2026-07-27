/**
 * The skills API: what is installed, and what one of them actually says.
 *
 * THE INVARIANT THIS HOLDS: **the filesystem is the source of truth, and this
 * endpoint reports it as it is — including the parts of it that are broken.** There
 * is no skills table (`db/schema.sql` says so out loud), nothing is cached, and a
 * listing is a fresh walk of the two source directories. So a skill dropped into
 * `~/.bough/skills` appears on the next request with no restart and no reindex, and
 * a skill whose SKILL.md is malformed is listed WITH its `error` rather than being
 * quietly omitted — the panel that shows the user their skills is the one place the
 * mistake is discoverable before a turn silently runs without it.
 *
 * WHY A BODY ROUTE. `GET /skills/:name` answers the two questions the list cannot:
 * what will actually be appended to the prompt, and what `${SKILL_DIR}` resolved
 * to. Both are answers about THIS install's paths, and the alternative to serving
 * them is a user reading a file they have to locate first.
 *
 * There is no POST, PUT or DELETE, and that is a decision rather than a gap: a skill
 * is a folder with a markdown file in it, authored in an editor (or by the agent,
 * which already has `write`). An HTTP CRUD surface over it would be a second way to
 * write files with none of the properties of the first.
 */
import { NotFoundError } from "../errors.ts";
import { defaultSources, listSkills, loadSkill, type Skill } from "../skills/skills.ts";
import { type Handler, json } from "./http.ts";

/** One row of `GET /skills`. The body is deliberately not in the listing. */
export interface SkillRow {
  name: string;
  description: string;
  source: Skill["source"];
  /** The skill's folder — what `${SKILL_DIR}` resolves to inside its body. */
  dir: string;
  /** MCP servers invoking it grants. Omitted when it needs none. */
  mcp?: string[];
  /** Present when the SKILL.md could not be parsed; the skill will not load. */
  error?: string;
}

function row(skill: Skill): SkillRow {
  return {
    name: skill.name,
    description: skill.description,
    source: skill.source,
    dir: skill.dir,
    ...(skill.mcp.length > 0 ? { mcp: skill.mcp } : {}),
    ...(skill.error ? { error: skill.error } : {}),
  };
}

/**
 * `GET /skills` — every installed skill, name-sorted, first source winning.
 *
 * `sources` rides along because "why is my skill not listed?" is almost always
 * answered by the directory it was expected in, and a client that only ever sees an
 * empty array cannot tell "nothing installed" from "looking in the wrong place".
 */
export const listSkillsH: Handler = () =>
  json({
    skills: listSkills().map(row),
    sources: defaultSources(),
  });

/**
 * `GET /skills/:name` — one skill, body included, `${SKILL_DIR}` already resolved.
 *
 * A 404 names the alternatives, because the usual cause is a typo or a folder that
 * has no SKILL.md in it — both of which the list makes obvious the moment it is in
 * front of you.
 */
export const getSkillH: Handler = (_req, _ctx, params) => {
  const skill = loadSkill(params.name);
  if (!skill) {
    const installed = listSkills().map((s) => `/${s.name}`);
    throw new NotFoundError(
      `no skill "${params.name}". A skill is a folder <dir>/${params.name}/SKILL.md in ` +
        `one of ${defaultSources().map((s) => s.dir).join(" or ")}. ` +
        (installed.length > 0 ? `Installed: ${installed.join(", ")}.` : `Nothing is installed.`),
    );
  }
  return json({ ...row(skill), body: skill.body });
};
