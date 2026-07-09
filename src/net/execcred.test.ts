import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { execBearerProvider } from "./execcred.ts";
import type { ExecCredSpec } from "./kubeconfig.ts";

// These shell out (the provider runs the exec plugin), so they self-skip without
// --allow-run — same posture as the seatbelt enforcement smokes.
const canRun = (await Deno.permissions.query({ name: "run" })).state === "granted";

/** A fake exec plugin: /bin/sh printing an ExecCredential with the given token. */
function fakePlugin(token: string, expiresInMs?: number): ExecCredSpec {
  const status = expiresInMs === undefined
    ? { token }
    : { token, expirationTimestamp: new Date(Date.now() + expiresInMs).toISOString() };
  return {
    host: "cluster.example.com",
    command: "/bin/sh",
    args: ["-c", `echo '${JSON.stringify({ kind: "ExecCredential", status })}'`],
    env: {},
  };
}

Deno.test({
  name: "execBearerProvider: mints once and serves from cache until expiry",
  ignore: !canRun,
  fn: async () => {
    const dir = await Deno.makeTempDir({ prefix: "bough-execcred-" });
    try {
      // count invocations via a side-effect file, since the command is a real subprocess
      const spec: ExecCredSpec = {
        host: "cluster.example.com",
        command: "/bin/sh",
        args: [
          "-c",
          `echo x >> '${dir}/calls'; echo '{"status":{"token":"tok-1"}}'`,
        ],
        env: {},
      };
      const provider = execBearerProvider(spec);
      assertEquals(await provider(), "Bearer tok-1");
      assertEquals(await provider(), "Bearer tok-1");
      const calls = (await Deno.readTextFile(`${dir}/calls`)).trim().split("\n").length;
      assertEquals(calls, 1); // second hit came from cache (no expiry → default TTL)
    } finally {
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  name: "execBearerProvider: an already-expired token is re-minted on the next call",
  ignore: !canRun,
  fn: async () => {
    // expirationTimestamp in the past → the cache entry is stale immediately
    const provider = execBearerProvider(fakePlugin("tok", -120_000));
    assertEquals(await provider(), "Bearer tok");
    assertEquals(await provider(), "Bearer tok"); // re-mints, same fake output
  },
});

Deno.test({
  name: "execBearerProvider: plugin failure surfaces its stderr",
  ignore: !canRun,
  fn: async () => {
    const provider = execBearerProvider({
      host: "cluster.example.com",
      command: "/bin/sh",
      args: ["-c", "echo 'sso session expired' >&2; exit 1"],
      env: {},
    });
    const err = await assertRejects(() => provider(), Error);
    assertStringIncludes(err.message, "exited 1");
    assertStringIncludes(err.message, "sso session expired");
  },
});

Deno.test({
  name: "execBearerProvider: output without a token rejects",
  ignore: !canRun,
  fn: async () => {
    const provider = execBearerProvider({
      host: "cluster.example.com",
      command: "/bin/sh",
      args: ["-c", "echo '{}'"],
      env: {},
    });
    const err = await assertRejects(() => provider(), Error);
    assertStringIncludes(err.message, "no ExecCredential token");
  },
});
