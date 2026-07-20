/**
 * Command shims prepended to a sandboxed shell's PATH for tools that CANNOT be
 * made to work via the Seatbelt profile.
 *
 * The one current case: `/bin/ps` is setuid root, and the macOS kernel refuses
 * to exec setuid binaries inside a sandbox — even an `(allow default)` profile
 * fails with EPERM, so no profile rule can help. `pgrep` is not setuid and
 * reads the same process table, so the shim approximates `ps` with it: liveness
 * checks (`ps -p <pid>`) keep their exit-code contract; anything else gets the
 * full "PID + command" listing with other flags ignored, loudly labeled so the
 * agent knows what it's looking at.
 */
import { join } from "node:path";

const PS_SHIM = `#!/bin/sh
# bough sandbox shim: /bin/ps is setuid root and macOS refuses to exec setuid
# binaries inside the Seatbelt sandbox, so this approximates ps via pgrep.
# -a: BSD pgrep excludes itself AND its ancestors by default, which would hide
# the calling shell from its own liveness check.
if [ "$1" = "-p" ] && [ -n "$2" ]; then
  out=$(/usr/bin/pgrep -a -lf . | /usr/bin/awk -v p="$2" '$1 == p')
  [ -n "$out" ] || exit 1
  printf '  PID COMMAND\\n%s\\n' "$out"
  exit 0
fi
echo "  PID COMMAND   (bough ps shim: setuid /bin/ps cannot run sandboxed; full listing via pgrep, ps flags ignored)"
exec /usr/bin/pgrep -a -lf .
`;

function shimDir(): string {
  const home = Deno.env.get("HOME");
  if (!home) throw new Error("shims: no $HOME");
  return join(home, ".bough", "shims");
}

let ensured: Promise<string> | null = null;

/**
 * Write the shim dir (idempotent, host-side) and return its path for PATH
 * prepending. Cached per process; a failure clears the cache so the next shell
 * retries rather than pinning a broken PATH entry.
 */
export function ensureShims(): Promise<string> {
  ensured ??= (async () => {
    const dir = shimDir();
    try {
      await Deno.mkdir(dir, { recursive: true });
      const ps = join(dir, "ps");
      const current = await Deno.readTextFile(ps).catch(() => null);
      if (current !== PS_SHIM) {
        await Deno.writeTextFile(ps, PS_SHIM);
        await Deno.chmod(ps, 0o755);
      }
      return dir;
    } catch (e) {
      ensured = null;
      throw e;
    }
  })();
  return ensured;
}
