/**
 * Containment probes for the Seatbelt filesystem sandbox — the load-bearing test:
 * run escape attempts inside a `wrap()`-ed subprocess and assert each is DENIED,
 * with a "positive control" guard so a sandbox that trivially breaks all I/O
 * can't false-pass every deny.
 *
 * Scope note: network probes (direct HTTPS, raw TCP, UDP DNS, cloud-metadata
 * SSRF, no-proxy egress) belong to the Claw Patrol egress layer (`src/net/**`),
 * NOT to Seatbelt — Seatbelt does not restrict the network here. This file
 * therefore covers the filesystem/process half: write-confinement (deny-write
 * outside the workspace) and read-confinement (the credential/secret read
 * denylist). The network probes should be ported
 * against Claw Patrol as its own containment test.
 *
 * Safety: probes never target real credential files. Write-escapes use unique
 * `$HOME/.bough-probe-*` names (cleaned up); read-confinement uses a synthetic
 * secret added via `denyRead`, plus the real shipped denylist entry `~/.zshrc`
 * only when it exists (a read, so non-destructive). macOS-only; self-skips
 * without `--allow-run`.
 */
import { assert, assertEquals } from "jsr:@std/assert@1";
import { wrap } from "./seatbelt.ts";

async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}

const smokeOk = Deno.build.os === "darwin" && (await canRun("/usr/bin/sandbox-exec"));

/** Run `cmd` in `/bin/sh` under the seatbelt profile for `ws`; return the exit code. */
async function sandboxed(cmd: string, ws: string, denyRead?: string[]): Promise<number> {
  const argv = wrap(["/bin/sh", "-c", cmd], { workspace: ws, denyRead });
  const { code } = await new Deno.Command(argv[0], {
    args: argv.slice(1),
    stdout: "null",
    stderr: "null",
  }).output();
  return code;
}

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

Deno.test({
  name: "seatbelt containment probes: filesystem escapes are denied",
  ignore: !smokeOk,
  fn: async (t) => {
    // realPath resolves /var → /private/var so the profile's subpath rules match
    // the kernel's canonicalized paths (real bough workspaces are already canonical).
    const ws = await Deno.realPath(await Deno.makeTempDir({ prefix: "probe-ws-" }));
    const secretDir = await Deno.realPath(await Deno.makeTempDir({ prefix: "probe-sec-" }));
    const secret = `${secretDir}/id_rsa`;
    await Deno.writeTextFile(secret, "PRIVATE KEY MATERIAL\n");
    const home = Deno.env.get("HOME")!;

    const homeFile = `${home}/.bough-probe-${crypto.randomUUID()}`;
    const homeDir = `${home}/.bough-probe-dir-${crypto.randomUUID()}`;
    const zshrc = `${home}/.zshrc`;

    try {
      // --- positive controls: legit I/O must succeed, else every deny false-passes.
      await t.step("control: write inside the workspace succeeds", async () => {
        assertEquals(await sandboxed(`echo ok > '${ws}/inside.txt'`, ws), 0);
        assertEquals((await Deno.readTextFile(`${ws}/inside.txt`)).trim(), "ok");
      });

      await t.step("control: reading a non-denied file succeeds", async () => {
        // Same secret file, but WITHOUT denyRead — proves reads aren't globally broken.
        assertEquals(await sandboxed(`cat '${secret}'`, ws), 0);
      });

      // --- write-confinement escapes: normally user-writable, must be denied.
      await t.step("escape: write to $HOME root is denied", async () => {
        const code = await sandboxed(`echo pwned > '${homeFile}'`, ws);
        assert(code !== 0, "write to $HOME must fail");
        assert(!(await exists(homeFile)), "escape file must not be created");
      });

      await t.step("escape: creating a dir outside the workspace is denied", async () => {
        const code = await sandboxed(`mkdir '${homeDir}'`, ws);
        assert(code !== 0, "mkdir outside workspace must fail");
        assert(!(await exists(homeDir)), "escape dir must not be created");
      });

      await t.step("escape: appending outside the workspace is denied", async () => {
        const code = await sandboxed(`echo more >> '${homeFile}'`, ws);
        assert(code !== 0, "append outside workspace must fail");
        assert(!(await exists(homeFile)), "escape file must not be created");
      });

      // --- read-confinement: the credential/secret denylist blocks reads.
      await t.step("escape: reading a denylisted secret is denied", async () => {
        const code = await sandboxed(`cat '${secret}'`, ws, [secretDir]);
        assert(code !== 0, "read of a denylisted path must fail");
      });

      // --- the real shipped denylist (~/.zshrc), only if present (read = non-destructive).
      if (await exists(zshrc)) {
        await t.step("escape: reading ~/.zshrc (shipped denylist) is denied", async () => {
          const code = await sandboxed(`cat '${zshrc}'`, ws);
          assert(code !== 0, "read of ~/.zshrc must fail under the default profile");
        });
      }
    } finally {
      await Deno.remove(ws, { recursive: true }).catch(() => {});
      await Deno.remove(secretDir, { recursive: true }).catch(() => {});
      // If any escape leaked (sandbox regression), don't leave litter in $HOME.
      await Deno.remove(homeFile).catch(() => {});
      await Deno.remove(homeDir, { recursive: true }).catch(() => {});
    }
  },
});
