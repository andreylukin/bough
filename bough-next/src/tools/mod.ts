/**
 * The default tool set the supervisor exposes to Claude: run_steps ONLY (SPEC §5 —
 * the supervisor plans and writes; the harness executes). The four primitives stay
 * exported as modules because they ARE the host functions run_steps bridges into
 * the sandbox, and tests/fakes may still inject them directly.
 */
import type { ToolDef } from "./types.ts";
import { runSteps } from "./run_steps.ts";

export type { ToolDef, ToolRunCtx } from "./types.ts";
export { jsonSchema } from "./types.ts";
export { DONE_ACCEPTED, DONE_REJECTED, runSteps } from "./run_steps.ts";

export const defaultTools: ToolDef[] = [runSteps];
