/**
 * What ⌘v hands the composer: a picture, or words.
 *
 * THE ORDER IS THE WHOLE POINT. This used to read `pbpaste` first and return its
 * text whenever it was non-empty, which is exactly backwards for the gesture it
 * serves. A macOS pasteboard holding an image almost always ALSO holds text —
 * copying a file in Finder puts its path (or `file://` URL) on as a string, and
 * several apps put a filename beside their image data. So "copy image, ⌘v" put
 * the PATH in the composer and the model was sent a line of prose about a file it
 * could not open. The image data is the more specific offer; it is read first,
 * and text is the fallback rather than the winner.
 *
 * SECOND — a pasteboard whose text IS a path to an image file is a picture too.
 * That is what Finder's Copy gives (no image data at all, just the file), and it
 * is the case the user actually hits. The file is read here, so what crosses to
 * the server is bytes like any other paste; nothing downstream learns a second
 * shape. Anything else — a path to a non-image, a file that is gone, more than
 * one line — stays text, because guessing wrong would swallow a paste the user
 * meant as words.
 */
import { readFile, stat } from "node:fs/promises";
import { isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";

export type Clipboard = { image: Blob } | { text: string } | null;

/** The four the providers accept, keyed by the extension a pasteboard path carries. */
const MEDIA_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

/**
 * The image file a clipboard's TEXT names, or `null` if the text is just text.
 *
 * Pure and total: it decides from the string alone and never touches disk, so the
 * rule can be pinned by tests without a pasteboard. A `file://` URL that does not
 * parse is text, like every other thing that is not a path we recognise.
 */
export function clipboardImagePath(text: string): { path: string; mediaType: string } | null {
  const one = text.trim();
  if (one === "" || one.includes("\n")) return null;
  let path = one;
  if (one.startsWith("file://")) {
    try { path = fileURLToPath(one); } catch { return null; }
  }
  if (!isAbsolute(path)) return null;
  const mediaType = MEDIA_TYPES[path.slice(path.lastIndexOf(".") + 1).toLowerCase()];
  return mediaType ? { path, mediaType } : null;
}

/**
 * Read that file as a paste, or fall back to the text if it cannot be read.
 *
 * A missing or unreadable file is NOT an error here: the string may simply have
 * been a path the user meant to type. Handing back the text is what they asked
 * for either way.
 */
export async function clipboardFromText(text: string): Promise<Clipboard> {
  const named = clipboardImagePath(text);
  if (!named) return { text };
  try {
    if (!(await stat(named.path)).isFile()) return { text };
    return { image: new Blob([await readFile(named.path)], { type: named.mediaType }) };
  } catch {
    return { text };
  }
}
