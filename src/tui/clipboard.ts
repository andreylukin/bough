import { osc52Copy } from "./term.ts";

// Clipboard write via pbcopy (bough is macOS-only; the TUI runs with
// --allow-run). If pbcopy is unreachable (e.g. the TUI is running over SSH),
// fall back to an OSC 52 escape — the sequence travels the connection and lands
// on the clipboard of the terminal the user is actually sitting at.
export async function copyToClipboard(text: string): Promise<void> {
  try {
    const child = new Deno.Command("pbcopy", { stdin: "piped", stdout: "null", stderr: "null" })
      .spawn();
    const w = child.stdin.getWriter();
    await w.write(new TextEncoder().encode(text));
    await w.close();
    const { success } = await child.status;
    if (!success) throw new Error("pbcopy failed");
  } catch {
    osc52Copy(text); // best-effort: the terminal gives no ack either way
  }
}
