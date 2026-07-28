/**
 * `image()` (T6.4).
 *
 * TWO THINGS THIS FILE PINS:
 *
 *   1. **The picture arrives on the NEXT turn.** The attach posts a system note
 *      carrying the image part; nothing is injected into the running program or the
 *      turn's already-assembled thread, and the confirmation string says so.
 *   2. **A file that cannot be attached throws CATCHABLY, naming which limit it hit.**
 *      Missing, unsupported, too large and unreadable are four different next moves,
 *      so they are four different messages.
 *
 * Hermetic: attachments land in a per-test temp directory, never `~/.bough`, and the
 * files are a handful of bytes with the right extension — `attachImage` judges
 * extension and size, never pixels, so no real image is needed.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve. (Same constraint `hostfn/shell.test.ts` and `bus.test.ts` document.)
 */

import { test } from "bun:test";
import assert from "node:assert";
import {
  closeSync,
  ftruncateSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { ProgramError } from "../errors.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { ImagePart, Message, Session } from "../schema/parts.ts";
import type { TurnCtx } from "../types.ts";
import {
  attachImage,
  createImageHostFn,
  type ImageDeps,
  imageMediaType,
  imageNoteText,
  MAX_IMAGE_BYTES,
  resolveImagePath,
} from "./image.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

interface Fixture {
  db: SqliteDb;
  ctx: TurnCtx;
  session: Session;
  events: BoughEvent[];
  dir: string;
  dest: string;
  close: () => void;
}

function fixture(): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const dir = mkdtempSync(join(tmpdir(), "bough-image-"));
  const dest = join(dir, "attachments");
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "s",
    kind: "root",
    parentId: null,
    createdAt: Date.now(),
    workspace: dir,
    originDir: dir,
  });
  const ctx: TurnCtx = {
    db,
    bus,
    sessionId: session.id,
    turnId: "turn-1",
    messageId: "message-1",
    workspace: dir,
    model: "test-model",
    signal: new AbortController().signal,
    depth: 0,
  };
  return {
    db,
    ctx,
    session,
    events,
    dir,
    dest,
    close: () => {
      db.close();
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

/** Write `bytes` bytes into `name` under the fixture's workspace. */
function writeFile(dir: string, name: string, bytes = 8): string {
  const path = join(dir, name);
  writeFileSync(path, new Uint8Array(bytes).fill(1));
  return path;
}

function rejects(fn: () => unknown, fragment: string): void {
  try {
    fn();
  } catch (err) {
    assert.ok(err instanceof ProgramError, `expected ProgramError, got ${err}`);
    assert.ok(
      err.message.includes(fragment),
      `expected message to mention ${JSON.stringify(fragment)}, got: ${err.message}`,
    );
    return;
  }
  assert.fail("expected a ProgramError");
}

// ---------------------------------------------------------------------------
// Types and paths
// ---------------------------------------------------------------------------

test("imageMediaType covers png/jpg/gif/webp and nothing else", () => {
  assert.equal(imageMediaType("a.png"), "image/png");
  assert.equal(imageMediaType("a.PNG"), "image/png");
  assert.equal(imageMediaType("a.jpg"), "image/jpeg");
  assert.equal(imageMediaType("a.jpeg"), "image/jpeg");
  assert.equal(imageMediaType("a.gif"), "image/gif");
  assert.equal(imageMediaType("a.webp"), "image/webp");
  for (const bad of ["a.svg", "a.pdf", "a.txt", "a.heic", "noextension"]) {
    assert.equal(imageMediaType(bad), null, bad);
  }
});

test("paths resolve absolute, ~/-relative, or against the workspace", () => {
  assert.equal(resolveImagePath("/tmp/a.png", "/work", "/home/u"), "/tmp/a.png");
  assert.equal(resolveImagePath("~/shots/a.png", "/work", "/home/u"), "/home/u/shots/a.png");
  assert.equal(resolveImagePath("out/a.png", "/work", "/home/u"), "/work/out/a.png");
  // Not confined to the workspace: a screenshot usually lives outside the checkout,
  // and the program could read it directly anyway (spec §2).
  assert.equal(resolveImagePath("../a.png", "/work/repo", "/home/u"), "/work/a.png");
});

// ---------------------------------------------------------------------------
// The attach
// ---------------------------------------------------------------------------

test("attachImage copies the bytes and stores the COPY's path", () => {
  const f = fixture();
  const src = writeFile(f.dir, "shot.png", 12);
  const result = attachImage(src, "shot.png", f.dest);
  assert.ok(result.ok);

  const part = result.part;
  assert.equal(part.type, "image");
  assert.equal(part.mediaType, "image/png");
  assert.equal(part.name, "shot.png", "the name is what the program spelled, not the abs path");
  assert.equal(part.size, 12);
  assert.notEqual(part.path, src);
  assert.equal(statSync(part.path).size, 12);

  // The durability property: the original can go and the attachment stays.
  rmSync(src);
  assert.equal(statSync(part.path).size, 12);
  f.close();
});

test("two attachments of the same filename do not overwrite each other", () => {
  const f = fixture();
  const a = writeFile(f.dir, "shot.png", 4);
  const first = attachImage(a, "shot.png", f.dest);
  writeFileSync(a, new Uint8Array(9).fill(2));
  const second = attachImage(a, "shot.png", f.dest);
  assert.ok(first.ok && second.ok);
  assert.notEqual(first.part.path, second.part.path);
  assert.equal(statSync(first.part.path).size, 4);
  assert.equal(statSync(second.part.path).size, 9);
  f.close();
});

test("attachImage reports each refusal distinctly and never throws", () => {
  const f = fixture();
  assert.deepEqual(attachImage(join(f.dir, "a.svg"), "a.svg", f.dest), {
    ok: false,
    reason: "unsupported",
  });
  assert.deepEqual(attachImage(join(f.dir, "gone.png"), "gone.png", f.dest), {
    ok: false,
    reason: "missing",
  });
  mkdirSync(join(f.dir, "dir.png"));
  assert.deepEqual(attachImage(join(f.dir, "dir.png"), "dir.png", f.dest), {
    ok: false,
    reason: "not-a-file",
  });
  f.close();
});

test("a file over 5MB is refused", () => {
  const f = fixture();
  const src = join(f.dir, "big.png");
  // Sparse-ish: one byte written at the far end gives the size without the memory.
  const file = openSync(src, "w");
  ftruncateSync(file, MAX_IMAGE_BYTES + 1);
  closeSync(file);
  const result = attachImage(src, "big.png", f.dest);
  assert.equal(result.ok, false);
  assert.equal(result.ok === false && result.reason, "too-large");
  f.close();
});

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

test("image() posts a system note carrying the image part", () => {
  const f = fixture();
  writeFile(f.dir, "chart.png", 20);
  const { image } = createImageHostFn(f.ctx, { destDir: f.dest });

  const confirmation = image!("chart.png", "the rendered chart");

  return confirmation.then((text) => {
    // The confirmation must say NEXT turn, or the model writes a polling loop.
    assert.match(text, /NEXT/);
    assert.match(text, /chart\.png/);

    const messages = f.db.messagesFor(f.session.id);
    assert.equal(messages.length, 1);
    const note: Message = messages[0];
    assert.equal(note.role, "system");
    assert.equal(note.pending, false);
    assert.equal(note.parts.length, 2);
    assert.deepEqual(note.parts[0], {
      type: "text",
      text: imageNoteText("chart.png", "the rendered chart"),
    });

    const part = note.parts[1] as ImagePart;
    assert.equal(part.type, "image");
    assert.equal(part.name, "chart.png");
    assert.equal(part.mediaType, "image/png");
    assert.equal(statSync(part.path).size, 20);

    // Nothing was injected into the running turn's own message — the note is a new
    // message, which is what makes it arrive next turn.
    assert.notEqual(note.id, f.ctx.messageId);
    assert.deepEqual(f.events.map((e) => e.type), ["message.started"]);
    f.close();
  });
});

test("the note omits the dash when there is no note text", async () => {
  const f = fixture();
  writeFile(f.dir, "a.png");
  const { image } = createImageHostFn(f.ctx, { destDir: f.dest });
  await image!("a.png");
  assert.equal(
    (f.db.messagesFor(f.session.id)[0].parts[0] as { text: string }).text,
    "[image] a.png",
  );
  f.close();
});

test("image() resolves an absolute path and a ~/ one", async () => {
  const f = fixture();
  const abs = writeFile(f.dir, "abs.png", 5);
  const home = join(f.dir, "home");
  mkdirSync(home);
  writeFile(home, "tilde.png", 6);

  const { image } = createImageHostFn(f.ctx, { destDir: f.dest, home });
  await image!(abs);
  await image!("~/tilde.png");

  const sizes = f.db.messagesFor(f.session.id)
    .map((m) => (m.parts[1] as ImagePart).size);
  assert.deepEqual(sizes, [5, 6]);
  f.close();
});

test("each refusal is a catchable ProgramError naming its own fix", () => {
  const f = fixture();
  const { image } = createImageHostFn(f.ctx, { destDir: f.dest });

  rejects(() => image!("gone.png"), "does not exist");
  writeFile(f.dir, "notes.txt");
  rejects(() => image!("notes.txt"), "not a supported image type");

  const big = join(f.dir, "big.jpg");
  const file = openSync(big, "w");
  ftruncateSync(file, MAX_IMAGE_BYTES + 1);
  closeSync(file);
  rejects(() => image!("big.jpg"), "over the");

  // Nothing was written for any of them.
  assert.equal(f.db.messagesFor(f.session.id).length, 0);
  f.close();
});

test("image() posts through the injected seam so a caller can observe it", async () => {
  const f = fixture();
  writeFile(f.dir, "a.png");
  const posted: { sessionId: string; text: string; extra: unknown }[] = [];
  const post: ImageDeps["post"] = (_ctx, sessionId, text, deps) => {
    posted.push({ sessionId, text, extra: deps?.extra });
    return { message: null, wake: "recorded" };
  };
  const { image } = createImageHostFn(f.ctx, { destDir: f.dest, post });
  await image!("a.png");
  assert.equal(posted.length, 1);
  assert.equal(posted[0].sessionId, f.session.id);
  assert.equal(posted[0].text, "[image] a.png");
  assert.equal((posted[0].extra as ImagePart[])[0].type, "image");
  f.close();
});
