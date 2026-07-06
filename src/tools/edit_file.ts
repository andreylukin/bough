/** Exact-string replace in a file. Requires a unique match, like a surgical edit. */
import { z } from "zod/v4";
import { resolveInWorkspace, type ToolDef, type ToolRunCtx } from "./types.ts";
import { reconcileEdit } from "../worker/apply.ts";

const schema = z.object({
  path: z.string().describe("File path, absolute or relative to the workspace."),
  old_string: z.string().describe("Exact text to replace. Must appear exactly once."),
  new_string: z.string().describe("Replacement text."),
});

export const editFile: ToolDef = {
  name: "edit_file",
  description:
    "Replace an exact substring in a file. `old_string` must match exactly once, or the edit is " +
    "rejected — include enough surrounding context to make it unique.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { path, old_string, new_string } = input as z.infer<typeof schema>;
    const full = resolveInWorkspace(ctx, path);
    let text: string;
    try {
      text = await Deno.readTextFile(full);
    } catch (e) {
      throw new Error(`cannot read ${path}: ${(e as Error).message}`);
    }
    const count = old_string === "" ? 0 : text.split(old_string).length - 1;
    if (count === 0) {
      // Fast-apply: the local worker locates the drifted line range the edit meant
      // to match; deterministic checks in reconcileEdit decide. Null = fail as before.
      const reconciled = old_string === "" ? null : await reconcileEdit(text, old_string, new_string);
      if (reconciled !== null) {
        await Deno.writeTextFile(full, reconciled);
        return `edited ${path} (old_string was not an exact match; the local worker ` +
          `located the file's drifted text and the replacement was applied there — ` +
          `read the file if you need to verify)`;
      }
      throw new Error(`old_string not found in ${path}`);
    }
    if (count > 1) throw new Error(`old_string matches ${count} times in ${path}; make it unique`);
    await Deno.writeTextFile(full, text.replace(old_string, new_string));
    return `edited ${path}`;
  },
};
