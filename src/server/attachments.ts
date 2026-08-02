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
import { IMAGE_MEDIA_TYPES, MAX_IMAGE_BYTES } from "../hostfn/image.ts";
import { json, type Handler } from "./http.ts";

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
