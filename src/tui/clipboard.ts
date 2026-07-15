// Clipboard write via pbcopy (bough is macOS-only; the TUI runs with
// --allow-run). Rejects on a non-zero exit so callers can toast the failure.
export async function copyToClipboard(text: string): Promise<void> {
  const child = new Deno.Command("pbcopy", { stdin: "piped", stdout: "null", stderr: "null" })
    .spawn();
  const w = child.stdin.getWriter();
  await w.write(new TextEncoder().encode(text));
  await w.close();
  const { success } = await child.status;
  if (!success) throw new Error("pbcopy failed");
}
