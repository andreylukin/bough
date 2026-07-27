/**
 * Named workflows: a run whose script did what you wanted, kept as a command.
 *
 * WHY THIS EXISTS. Spec §8, "Saving a run": *"A run whose script did what you wanted
 * can be saved as a named workflow, at `~/.bough/workflows/saved/<name>.js`. A saved
 * workflow is invoked by name and parameterized through `args`, so an orchestration
 * worth repeating — a review you run on every branch — becomes a command rather than a
 * script you re-derive."* Without this a good orchestration survives only as a run id
 * somebody has to remember and rerun, which makes it a piece of history rather than a
 * tool; and `rerun` is the wrong verb for "do this again on a different branch",
 * because a rerun replays the journal it was told to seed from.
 *
 * THE INVARIANT THIS HOLDS: **a name can only ever address a file inside
 * `~/.bough/workflows/saved/`.** Names arrive in a URL path and in a request body — the
 * two least trustworthy inputs the server has — and every one of them is spent building
 * a filesystem path. So every path here is produced by exactly one function,
 * `savedPath`, which validates the shape of the name for a good error message and then
 * hands the RELATIVE name to `confine()` (`paths.ts`) as the backstop that decides.
 * Both, in that order, because they fail differently: the charset check tells a caller
 * what a name may contain, and `confine` catches everything a charset check forgets —
 * `..`, an absolute path, a URL-escaped separator that only becomes one after decoding.
 *
 * The relative name is what is confined, never the joined path: `join()` swallows a
 * leading slash, so `/etc/crontab` would land back under the saved directory and pass a
 * check made after the join. Same rule the script mirror follows (`workflow/journal.ts`).
 *
 * WHAT THIS IS NOT. Not a security boundary — programs run as the user and write any
 * file they like (spec §2). What it stops is the case it can stop: a name in a request
 * steering the SERVER's own path construction out of the store it meant to use.
 *
 * WHAT IS NOT HERE. Starting a run. This module reads and writes files and reads the
 * database; the engine, the subagent runner and the meta validation live behind
 * `workflow/control.ts`, and the route composes the two. That keeps this file pure
 * filesystem math — drivable with no worker, no LLM and no engine — and keeps saving a
 * workflow independent of whether one can currently be started.
 */

import { BadRequestError, NotFoundError } from "../errors.ts";
import { confine, workflowsDir } from "../paths.ts";
import type { Db } from "../types.ts";
import { readMirror } from "./journal.ts";
import { extractMeta } from "./meta.ts";

/** `~/.bough/workflows/saved` — beside the per-run mirrors, not inside them. */
export function savedDir(): string {
  return confine(workflowsDir(), "saved");
}

/** The longest name that still reads as a command. Arbitrary, and stated once. */
const MAX_NAME = 64;

/**
 * A name that may be typed, stored and logged: letters, digits, `.`, `_`, `-`, starting
 * with a letter or digit. Everything else — separators, spaces, leading dots, control
 * characters — is refused by name rather than silently rewritten, because a saved
 * workflow is addressed by the string the user typed.
 */
const NAME_SHAPE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

/**
 * Normalize a caller's name: trims, drops one trailing `.js` so `save("audit.js")` and
 * `save("audit")` name the same workflow rather than `audit.js.js`.
 */
export function normalizeName(raw: unknown): string {
  const name = String(raw ?? "").trim();
  return name.toLowerCase().endsWith(".js") ? name.slice(0, -3) : name;
}

/**
 * The absolute path for a saved workflow, or a 400 naming what is wrong with the name.
 *
 * Validate, then confine. The validation is the message; the confinement is the answer.
 */
export function savedPath(raw: unknown): string {
  const name = normalizeName(raw);
  if (!name) {
    throw new BadRequestError(
      "a saved workflow needs a name — POST {name: \"branch-review\"}. It becomes " +
        "~/.bough/workflows/saved/<name>.js and is how the workflow is invoked.",
    );
  }
  if (name.length > MAX_NAME) {
    throw new BadRequestError(
      `saved workflow name is ${name.length} characters, longer than the ${MAX_NAME} ` +
        `allowed — it is a command name, not a description. The description belongs in ` +
        `the script's \`meta\`.`,
    );
  }
  if (!NAME_SHAPE.test(name)) {
    throw new BadRequestError(
      `saved workflow name ${JSON.stringify(name)} is not usable — it may contain ` +
        `letters, digits, '.', '_' and '-', and must start with a letter or digit. ` +
        `Path separators and '..' are refused: a name addresses one file inside ` +
        `~/.bough/workflows/saved/, never a path.`,
    );
  }
  // The backstop, and the one that decides. Relative name, never the joined path.
  return confine(savedDir(), `${name}.js`);
}

/** A saved workflow as the API lists it. The script itself is only in the detail read. */
export interface SavedWorkflow {
  name: string;
  path: string;
  /** From the script's `meta`, when it has one. Empty otherwise — listing never fails. */
  description: string;
  bytes: number;
  updatedAt: number;
}

/** The saved workflow plus its script — what an invocation and an edit both need. */
export interface SavedWorkflowDetail extends SavedWorkflow {
  script: string;
}

/**
 * `meta.description` if the script has a valid one, else `""`.
 *
 * Deliberately swallowing: `meta` is validated when a run STARTS (`workflow/meta.ts`),
 * which is where a bad one must be refused. A listing that threw on one malformed file
 * would hide every other saved workflow behind it.
 */
function describe(script: string): string {
  try {
    return extractMeta(script).description;
  } catch {
    return "";
  }
}

/** Save a script under a name. Overwrites — a name is a command, not a version. */
export async function saveWorkflow(name: unknown, script: string): Promise<SavedWorkflow> {
  const path = savedPath(name);
  if (typeof script !== "string" || !script.trim()) {
    throw new BadRequestError(
      "a saved workflow needs a script — pass {script} directly, or {runId} to save the " +
        "script a finished run actually executed.",
    );
  }
  await Deno.mkdir(savedDir(), { recursive: true });
  await Deno.writeTextFile(path, script);
  const stat = await Deno.stat(path);
  return {
    name: normalizeName(name),
    path,
    description: describe(script),
    bytes: stat.size,
    updatedAt: stat.mtime?.getTime() ?? Date.now(),
  };
}

/**
 * Save the script a run actually ran, under a name.
 *
 * The MIRROR first, then the row — the same order a relaunch resolves them
 * (`workflow/journal.ts`). A user who edited `~/.bough/workflows/<id>.js` and relaunched
 * is saving the script that produced the result they liked, and saving the stored row
 * instead would quietly save the version they replaced.
 */
export async function saveRunAs(db: Db, runId: string, name: unknown): Promise<SavedWorkflow> {
  const run = db.getWorkflow(runId);
  if (!run) throw new NotFoundError(`workflow ${runId} not found`);
  // `savedPath` first so a bad name fails before anything is read.
  savedPath(name);
  const script = (await readMirror(runId)) ?? run.script;
  return await saveWorkflow(name, script);
}

/** Every saved workflow, by name. An absent directory lists empty, not an error. */
export async function listSavedWorkflows(): Promise<SavedWorkflow[]> {
  const dir = savedDir();
  const out: SavedWorkflow[] = [];
  const names: string[] = [];
  try {
    // The `try` must span the ITERATION, not the call: `Deno.readDir` returns an async
    // iterator and defers the open, so an absent directory throws on the first `next()`
    // rather than here. Wrapping only the call left "nothing saved yet" as an uncaught
    // NotFound out of a listing that is supposed to answer with an empty array.
    for await (const entry of Deno.readDir(dir)) {
      if (entry.isFile && entry.name.endsWith(".js")) names.push(entry.name.slice(0, -3));
    }
  } catch {
    return out; // nothing saved yet — the directory is created at boot and on first save
  }
  for (const name of names) {
    let path: string;
    try {
      path = savedPath(name);
    } catch {
      continue; // a file placed by hand under a name the API cannot address
    }
    try {
      const [script, stat] = await Promise.all([Deno.readTextFile(path), Deno.stat(path)]);
      out.push({
        name,
        path,
        description: describe(script),
        bytes: stat.size,
        updatedAt: stat.mtime?.getTime() ?? 0,
      });
    } catch {
      continue; // vanished or unreadable between the listing and the read
    }
  }
  return out.sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * One saved workflow, script included — the read an invocation makes.
 *
 * A missing file is a 404 naming the name, not an empty script: invoking a workflow
 * that is not there must not start an empty run.
 */
export async function readSavedWorkflow(name: unknown): Promise<SavedWorkflowDetail> {
  const path = savedPath(name);
  let script: string;
  try {
    script = await Deno.readTextFile(path);
  } catch {
    throw new NotFoundError(
      `no saved workflow named ${JSON.stringify(normalizeName(name))} — ` +
        `GET /saved-workflows lists what is saved, and POST /workflows/<id>/save {name} ` +
        `saves a run's script under one.`,
    );
  }
  const stat = await Deno.stat(path).catch(() => null);
  return {
    name: normalizeName(name),
    path,
    script,
    description: describe(script),
    bytes: stat?.size ?? script.length,
    updatedAt: stat?.mtime?.getTime() ?? 0,
  };
}

/** Remove a saved workflow. `false` when there was nothing under that name. */
export async function deleteSavedWorkflow(name: unknown): Promise<boolean> {
  const path = savedPath(name);
  try {
    await Deno.remove(path);
    return true;
  } catch {
    return false;
  }
}

/**
 * Create the saved directory at boot so `~/.bough/workflows/saved/` is a place the user
 * can drop a script into, not one that only appears after the first API save. Returns
 * how many workflows are there, for the boot line.
 */
export async function ensureSavedDir(): Promise<number> {
  try {
    await Deno.mkdir(savedDir(), { recursive: true });
  } catch {
    return 0; // read-only ~/.bough: saving will report its own error when tried
  }
  return (await listSavedWorkflows()).length;
}
