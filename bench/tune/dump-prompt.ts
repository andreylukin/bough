// Seed a prompt-variant dir from the built-in sections in src/supervisor/prompt.ts.
// usage: deno run --allow-read --allow-write --allow-env dump-prompt.ts <outdir>
//
// Must run WITHOUT BOUGH_PROMPT_DIR set — otherwise it would dump an override
// back out as if it were the built-in.
import { join } from "node:path";
import {
  SHIP_NOTE,
  SHIP_NOTE_WORKTREE,
  SYSTEM,
  SYSTEM_DELEGATION,
  SYSTEM_DELEGATION_NESTED,
  SYSTEM_SUBAGENT,
} from "../../src/supervisor/prompt.ts";

if (Deno.env.get("BOUGH_PROMPT_DIR")) {
  console.error("refusing to dump with BOUGH_PROMPT_DIR set (would copy an override, not the built-in)");
  Deno.exit(2);
}
const out = Deno.args[0];
if (!out) {
  console.error("usage: dump-prompt.ts <outdir>");
  Deno.exit(2);
}
Deno.mkdirSync(out, { recursive: true });
const sections: Record<string, string> = {
  "system.md": SYSTEM,
  "ship-note.md": SHIP_NOTE,
  "ship-note-worktree.md": SHIP_NOTE_WORKTREE,
  "delegation.md": SYSTEM_DELEGATION,
  "delegation-nested.md": SYSTEM_DELEGATION_NESTED,
  "subagent.md": SYSTEM_SUBAGENT,
};
for (const [name, text] of Object.entries(sections)) {
  Deno.writeTextFileSync(join(out, name), text.trim() + "\n");
}
console.log(`dumped ${Object.keys(sections).length} sections to ${out}`);
