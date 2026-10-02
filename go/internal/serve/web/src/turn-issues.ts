import type { Line } from "./types";
import { callFailed, callRunning, canceled, isNativeCall, thrownError, type Turn } from "./render";

/** A command's exit describes that command, not whether the user's request was completed. */
export function turnIssues(turn: Turn) {
  const failures = turn.body.filter((line) => !canceled(line) && (
    line.kind === "result" ? Boolean(thrownError(line)) || (typeof line.data?.exit === "number" && line.data.exit !== 0)
      : line.kind === "call" && isNativeCall(line) && !callRunning(line) && callFailed(line)
  ));
  const command = (line: Line) => isNativeCall(line) ? line.data?.tool === "bash" : typeof line.data?.exit === "number" && line.data.exit !== 0;
  const commands = failures.filter(command).length;
  const tools = failures.length - commands;
  const exit = turn.done?.data?.exit;
  // Old histories can name an exit without its call. Keep that evidence,
  // without blaming a hook or an unrelated command for it.
  const unknownExit = !turn.stopped && turn.done?.kind !== "cancelled" && typeof exit === "number" && exit !== 0 && !failures.some((line) => line.data?.exit === exit) ? exit : undefined;
  return { failures, commands, tools, unknownExit };
}
