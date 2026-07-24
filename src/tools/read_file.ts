/** Read a UTF-8 file, resolved relative to the session workspace. */
import { z } from "zod/v4";
import { resolveInGuest, resolveInWorkspace, type ToolDef, type ToolRunCtx } from "./types.ts";
import { readFile as vmReadFile } from "../sandbox/vm.ts";
import { ensureVm, machineName } from "../sandbox/vmsession.ts";
import {
  ensure as ensureAgentfs,
  readFile as agentfsReadFile,
  sandboxAgentfs,
} from "../sandbox/agentfs.ts";

const schema = z.object({
  path: z.string().describe("File path, absolute or relative to the workspace."),
});

export const readFile: ToolDef = {
  name: "read_file",
  description: "Read a text file and return its contents.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { path } = input as z.infer<typeof schema>;
    // agentfs overlay: the file may live only in the session's delta, so read it
    // through the overlay (host Deno.readTextFile sees the untouched real tree).
    // Paths are the same host paths the overlay copies-on-write.
    if (ctx.sandbox && ctx.sessionId && sandboxAgentfs()) {
      const full = resolveInWorkspace(ctx, path);
      ensureAgentfs(ctx.sessionId, { origin: ctx.workspace });
      try {
        return new TextDecoder().decode(await agentfsReadFile(ctx.sessionId, full));
      } catch (e) {
        throw new Error(`cannot read ${path}: ${(e as Error).message}`);
      }
    }
    // Guest-owned workspace: the file lives in the session VM, not on the host.
    if (ctx.guestFs) {
      const full = resolveInGuest(ctx, path);
      await ensureVm(ctx.guestFs.sessionId, { origin: ctx.workspace, gitOrigin: true });
      try {
        return new TextDecoder().decode(
          await vmReadFile(machineName(ctx.guestFs.sessionId), full),
        );
      } catch (e) {
        throw new Error(`cannot read ${path}: ${(e as Error).message}`);
      }
    }
    const full = resolveInWorkspace(ctx, path);
    try {
      return await Deno.readTextFile(full);
    } catch (e) {
      throw new Error(`cannot read ${path}: ${(e as Error).message}`);
    }
  },
};
