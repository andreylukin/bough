/**
 * macOS CA-trust guidance. Sandboxed curl/git trust bough's MITM CA via the CA
 * env vars (SSL_CERT_FILE etc.), but Go-based tools — `gh`, and some kubectl auth
 * plugins — ignore those on macOS and consult the system keychain instead. So the
 * CA has to be keychain-trusted ONCE for those tools to work through Claw Patrol.
 * This is Keychain Access / `security`, NOT a Network-panel extension (native Claw
 * Patrol has no network extension — it's an in-process proxy).
 *
 * We report whether the CA is already trusted (best-effort) and the exact one-time
 * command, so the UI can surface it only when it's actually needed.
 */

/** The one-time command that trusts bough's CA system-wide (Go tools then honor it). */
export function caTrustCommand(caPath: string): string {
  return `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain ${
    JSON.stringify(caPath)
  }`;
}

/**
 * Whether bough's CA already verifies under the system SSL trust policy. macOS only;
 * elsewhere the env-var CA path is enough, so we report "trusted" (nothing to do).
 * Best-effort: any failure to run `security` reports untrusted so the hint shows.
 */
export async function isCaTrusted(caPath: string): Promise<boolean> {
  if (Deno.build.os !== "darwin") return true;
  try {
    const { success } = await new Deno.Command("security", {
      args: ["verify-cert", "-p", "ssl", "-c", caPath],
      stdout: "null",
      stderr: "null",
    }).output();
    return success;
  } catch {
    return false;
  }
}
