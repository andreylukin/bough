/**
 * Clipboard-image intake for the native TUI.
 *
 * The invariant is that image bytes cross the loopback boundary once, are checked
 * before they touch disk, and thereafter messages carry only the durable path.
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { BadRequestError } from "../errors.ts";
import { attachmentsDir } from "../paths.ts";
import { json, type Handler } from "./http.ts";

/**
 * The providers' per-image cap, and the four formats every provider route accepts.
 *
 * They lived in `hostfn/image.ts` beside the `image()` verb, which is gone — a
 * program hands the model a picture by writing the file and letting the human
 * attach it, or not at all. This route is the remaining door: the user's own
 * clipboard paste. The limits are the MODEL's either way, which is why they are
 * enforced rather than inherited from whatever the terminal handed over.
 */
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

export const IMAGE_MEDIA_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

const TYPES: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
};

/** POST /attachments — copy one composer image into the durable attachment store. */
export const uploadAttachment: Handler = async (req) => {
  const mediaType = req.headers.get("content-type")?.split(";", 1)[0]?.toLowerCase() ?? "";
  const ext = TYPES[mediaType];
  if (!ext || IMAGE_MEDIA_TYPES[ext] !== mediaType) {
    throw new BadRequestError("unsupported image type: use PNG, JPEG, GIF, or WebP");
  }
  const bytes = new Uint8Array(await req.arrayBuffer());
  if (bytes.length === 0) throw new BadRequestError("image is empty");
  if (bytes.length > MAX_IMAGE_BYTES) {
    throw new BadRequestError("image is over the 5 MB limit; downscale or crop it first");
  }
  const dir = attachmentsDir();
  mkdirSync(dir, { recursive: true });
  const path = resolve(dir, crypto.randomUUID() + "." + ext);
  try {
    writeFileSync(path, bytes, { flag: "wx" });
  } catch {
    throw new BadRequestError("could not save clipboard image");
  }
  return json({ path, mediaType, name: `clipboard.${ext}`, size: bytes.length }, 201);
};
