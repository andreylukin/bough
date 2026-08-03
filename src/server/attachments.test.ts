/**
 * The clipboard-image intake, which is now the ONLY way a picture reaches the model.
 *
 * `image()` was the other one and is gone with the rest of the host verb, and its
 * tests went with it — taking the only coverage of `MAX_IMAGE_BYTES` and
 * `IMAGE_MEDIA_TYPES` along, since both lived beside the verb. They moved here, so
 * their enforcement is pinned here.
 *
 * WHAT IS UNDER TEST is the check-before-write order. The route validates the media
 * type and the size before `writeFileSync` runs, so a rejected upload leaves nothing
 * on disk — which is what lets these cases run against the real handler without a
 * temp directory or a `BOUGH_HOME` override.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { IMAGE_MEDIA_TYPES, MAX_IMAGE_BYTES, uploadAttachment } from "./attachments.ts";
import type { AppCtx } from "../types.ts";

const post = (mediaType: string, bytes: Uint8Array) =>
  new Request("http://127.0.0.1:4321/attachments", {
    method: "POST",
    headers: { "content-type": mediaType },
    // `.buffer` rather than the view: the DOM `BodyInit` union takes an
    // ArrayBuffer, and a Uint8Array only satisfies it under lib.dom's older shape.
    body: bytes.buffer as ArrayBuffer,
  });

/** The handler takes a ctx it never reads on these paths. */
const CTX = {} as AppCtx;

async function refusal(req: Request): Promise<string> {
  try {
    await uploadAttachment(req, CTX, {});
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("expected the upload to be refused");
}

test("the four formats every provider accepts, and nothing else", async () => {
  assert.deepEqual(Object.keys(IMAGE_MEDIA_TYPES).sort(), ["gif", "jpeg", "jpg", "png", "webp"]);
  // A PDF is a document a provider will not take as an image, and a SVG is markup
  // that would reach the model as a blank. Both are refused with the list.
  for (const type of ["application/pdf", "image/svg+xml", "text/plain", ""]) {
    assert.match(await refusal(post(type, new Uint8Array([1]))), /unsupported image type/);
  }
});

test("an oversized image is refused before it touches disk", async () => {
  // The cap is the MODEL's — a larger file cannot be sent at all — so it is enforced
  // rather than inherited from whatever the terminal handed over.
  const tooBig = new Uint8Array(MAX_IMAGE_BYTES + 1);
  assert.match(await refusal(post("image/png", tooBig)), /over the 5 MB limit/);
});

test("an empty body is its own refusal, not a zero-byte attachment", async () => {
  // A clipboard that held no image at all reads as a successful upload of nothing,
  // and the message would carry a path to a file with no picture in it.
  assert.match(await refusal(post("image/png", new Uint8Array())), /image is empty/);
});
