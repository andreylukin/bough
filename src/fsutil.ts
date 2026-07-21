/** True if `p` exists (file, dir, or anything else `stat` can see). */
export async function pathExists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}
