/** Create or overwrite a file, resolved relative to the session workspace. */
import { z } from "zod/v4";
import { dirname } from "node:path";
import { resolveInWorkspace, type ToolDef, type ToolRunCtx } from "./types.ts";
import {
  ensure as ensureAgentfs,
  sandboxAgentfs,
  writeFile as agentfsWriteFile,
} from "../sandbox/agentfs.ts";

const schema = z.object({
  path: z.string().describe("File path, absolute or relative to the workspace."),
  content: z.string().describe("The full contents to write."),
});

export const writeFile: ToolDef = {
  name: "write_file",
  description: "Write a text file, creating parent directories and overwriting any existing file.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { path, content } = input as z.infer<typeof schema>;
    // agentfs overlay: the file lands in the session's delta, not the real tree,
    // so route the write (and the parent-dir mkdir) through the overlay.
    if (ctx.sandbox && ctx.sessionId && sandboxAgentfs()) {
      const full = resolveInWorkspace(ctx, path);
      ensureAgentfs(ctx.sessionId, { origin: ctx.workspace });
      try {
        await agentfsWriteFile(ctx.sessionId, full, content);
      } catch (e) {
        throw new Error(`cannot write ${path}: ${(e as Error).message}`);
      }
      return `wrote ${content.length} bytes to ${path}`;
    }
    const full = resolveInWorkspace(ctx, path);
    try {
      await Deno.mkdir(dirname(full), { recursive: true });
      await Deno.writeTextFile(full, content);
    } catch (e) {
      throw new Error(`cannot write ${path}: ${(e as Error).message}`);
    }
    return `wrote ${content.length} bytes to ${path}`;
  },
};
