/**
 * `image(path, note?)` — how a program hands the model something to LOOK at.
 *
 * THE INVARIANT THIS HOLDS: **the picture arrives on the NEXT turn, never inside the
 * running program.** The thread the model is reasoning over was assembled once, at
 * turn start (`turn/runner.ts` builds `messages` before the first round); an image
 * attached mid-program cannot retroactively appear in a prompt that has already been
 * sent. So the attach posts a **system note** carrying the image part — the same wake
 * path a background shell's exit note uses (`agents/notes.ts`) — and the confirmation
 * string says so in as many words. A model told only "attached" writes a polling loop
 * waiting to see it, burns the rest of its round, and reports that the image never
 * arrived. Being told "you will see it on your next turn" ends the turn instead,
 * which is the correct move and the only one that works.
 *
 * THE SECOND RULE: **bytes never cross the bridge, and never enter the parts JSON.**
 * The host copies the file into `~/.bough/attachments/` and the part stores the
 * copy's path (spec §4). Two consequences worth stating, because both are the point:
 * message rows stay small however many screenshots a session accumulates, and the
 * message still replays after the program's own output file is overwritten or the
 * temp directory it lived in is swept — which, for a screenshot a program just
 * rendered into `/tmp`, is a matter of minutes.
 *
 * THE LIMITS ARE THE MODEL'S, NOT OURS. png/jpg/gif/webp and 5MB are what the
 * providers accept; a file outside them cannot be sent at all, so it is refused here
 * where the program can catch it and say something, rather than silently becoming a
 * blank in the next prompt.
 *
 * Each refusal names WHICH limit it hit (missing / not an image type / too large /
 * unreadable). The port lumped all four into one sentence listing every possibility,
 * which tells a model to guess: re-render as PNG, or downscale, or fix the path?
 * Error text is a product surface (spec §6), and these four call for four different
 * next moves.
 *
 * Ported from `src/turn.ts` (the `image()` tool ctx) and `src/server/files.ts`
 * (`attachImageFile`). Deltas from that port are marked `NOTE:`.
 */
import { copyFileSync, mkdirSync, type Stats, statSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, resolve } from "node:path";
import { postSystemNote } from "../agents/notes.ts";
import { ProgramError } from "../errors.ts";
import { attachmentsDir } from "../paths.ts";
import type { ImagePart } from "../schema/parts.ts";
import type { AppCtx, HostFns, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// The limits
// ---------------------------------------------------------------------------

/** The providers' per-image cap. A larger file cannot be sent, so it is refused. */
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

/** Extension → media type. The four formats every provider route accepts. */
export const IMAGE_MEDIA_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

/** The media type for a path, or `null` when the extension is not a supported one. */
export function imageMediaType(path: string): string | null {
  const dot = path.lastIndexOf(".");
  if (dot < 0) return null;
  return IMAGE_MEDIA_TYPES[path.slice(dot + 1).toLowerCase()] ?? null;
}

/**
 * Resolve the path the program named, exactly as the prompt section promises:
 * absolute, `~/`-relative, or relative to the workspace.
 *
 * Pure — `home` and `workspace` are parameters — and deliberately NOT confined to the
 * workspace. A screenshot almost always lives outside the checkout (`/tmp`, the
 * Desktop), and confinement here would buy nothing anyway: the program runs with the
 * user's full authority and could read the file itself (spec §2). `confine` guards
 * paths the *server* builds from request input, which this is not.
 */
export function resolveImagePath(path: string, workspace: string, home: string): string {
  const raw = path.trim();
  if (raw === "~") return home;
  if (raw.startsWith("~/")) return resolve(home, raw.slice(2));
  if (isAbsolute(raw)) return resolve(raw);
  return resolve(workspace, raw);
}

// ---------------------------------------------------------------------------
// The attach
// ---------------------------------------------------------------------------

/** Why an attach was refused, so the caller can say which limit was hit. */
export type AttachFailure = "unsupported" | "missing" | "not-a-file" | "too-large" | "unreadable";

export type AttachResult =
  | { ok: true; part: ImagePart }
  | { ok: false; reason: AttachFailure; detail?: string };

/**
 * Copy one image into the attachment store and describe it as an `ImagePart`.
 *
 * Copy first, store the copy's path second — never the original's. That ordering IS
 * the durability property: the part points at bytes only this server writes and only
 * this server deletes, so the message replays a week later even though the program's
 * output file is long gone.
 *
 * `name` is the path as the PROGRAM spelled it, not the resolved absolute one: that
 * is what the model asked for and what the transcript should show.
 *
 * Never throws — every failure is a typed result, because the caller (`image()`) and
 * a future composer `@ref` path want to do different things with the same refusals.
 */
export function attachImage(
  abs: string,
  name: string,
  destDir: string = attachmentsDir(),
): AttachResult {
  const mediaType = imageMediaType(abs);
  if (!mediaType) return { ok: false, reason: "unsupported" };

  let info: Stats;
  try {
    info = statSync(abs);
  } catch {
    return { ok: false, reason: "missing" };
  }
  if (!info.isFile()) return { ok: false, reason: "not-a-file" };
  if (info.size > MAX_IMAGE_BYTES) {
    return { ok: false, reason: "too-large", detail: String(info.size) };
  }

  try {
    mkdirSync(destDir, { recursive: true });
    // A random name, not the source's: two programs attaching `screenshot.png`
    // seconds apart must not overwrite each other's evidence.
    const ext = abs.slice(abs.lastIndexOf(".") + 1).toLowerCase();
    const dest = resolve(destDir, `${crypto.randomUUID()}.${ext}`);
    copyFileSync(abs, dest);
    return { ok: true, part: { type: "image", path: dest, mediaType, name, size: info.size } };
  } catch (err) {
    return { ok: false, reason: "unreadable", detail: (err as Error)?.message };
  }
}

/** The refusal message for each failure — what failed, and the move that fixes it. */
function refusal(path: string, failure: AttachResult & { ok: false }): string {
  switch (failure.reason) {
    case "unsupported":
      return `image(): ${path} is not a supported image type. Attach a .png, .jpg, ` +
        `.gif or .webp — re-render or convert it if you need to.`;
    case "missing":
      return `image(): ${path} does not exist. Paths are absolute, ~/-relative, or ` +
        `relative to the workspace; check that the file was actually written before ` +
        `attaching it.`;
    case "not-a-file":
      return `image(): ${path} is a directory, not an image file. Name the file itself.`;
    case "too-large":
      return `image(): ${path} is ${failure.detail} bytes, over the ${MAX_IMAGE_BYTES}-byte ` +
        `limit the model accepts. Downscale or crop it and attach the smaller file.`;
    default:
      return `image(): ${path} could not be read or copied${
        failure.detail ? ` (${failure.detail})` : ""
      }. Check permissions, or write a copy somewhere readable and attach that.`;
  }
}

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

/** The marker the UI keys off to collapse the note onto the image placeholder. */
export const IMAGE_NOTE_PREFIX = "[image]";

/** The system-note text for one attachment. Stable shape, not decoration. */
export function imageNoteText(path: string, note?: string): string {
  return `${IMAGE_NOTE_PREFIX} ${path}${note ? ` — ${note}` : ""}`;
}

export interface ImageDeps {
  /** Where copies land. Absent = `~/.bough/attachments`. Tests pass a temp dir. */
  destDir?: string;
  /** `~` expansion. Absent = the real home. */
  home?: string;
  /** Seam for the note post, so a test can observe it without a turn registry. */
  post?: typeof postSystemNote;
}

/**
 * Build the bridged `image` host function for one turn.
 *
 * The note is posted into the CURRENT session, which is mid-turn by definition, so
 * `postSystemNote`'s wake rule queues it and the running turn's drain picks it up —
 * the picture lands on the next turn with no extra machinery. The same call on an
 * idle session (a future non-turn caller) would start one, which is also right.
 */
export function createImageHostFn(ctx: TurnCtx, deps: ImageDeps = {}): Pick<HostFns, "image"> {
  return {
    image: (path: string, note?: string): Promise<string> => {
      const home = deps.home ?? homedir();
      const abs = resolveImagePath(path, ctx.workspace, home);
      const attached = attachImage(abs, path, deps.destDir);
      if (!attached.ok) {
        // A rejected host call is an ordinary catchable exception inside the program
        // (`harness/protocol.ts`), which is what the prompt section tells the model
        // to expect — so a failed attach costs a `catch`, not the whole round.
        throw new ProgramError(refusal(path, attached));
      }

      (deps.post ?? postSystemNote)(
        ctx as AppCtx,
        ctx.sessionId,
        imageNoteText(path, note),
        { extra: [attached.part] },
      );

      return Promise.resolve(
        `attached ${path} (${attached.part.size} bytes). You will see it on your NEXT ` +
          `turn — end this one rather than waiting for it here.`,
      );
    },
  };
}
