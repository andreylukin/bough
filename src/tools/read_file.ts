/** Read a UTF-8 file, resolved relative to the session workspace. */
import { z } from "zod/v4";
import { resolveInWorkspace, type ToolDef, type ToolRunCtx } from "./types.ts";

const schema = z.object({
  path: z.string().describe("File path, absolute or relative to the workspace."),
});

export const readFile: ToolDef = {
  name: "read_file",
  description: "Read a text file and return its contents.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { path } = input as z.infer<typeof schema>;
    const full = resolveInWorkspace(ctx, path);
    try {
      return await Deno.readTextFile(full);
    } catch (e) {
      throw new Error(`cannot read ${path}: ${(e as Error).message}`);
    }
  },
};
