import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { buildProfile, wrap } from "./seatbelt.ts";

const GOLDEN = `(version 1)
(allow default)

;; deny reads of credential/secret/private paths
(deny file-read*
  (subpath "/home/u/.ssh")
  (subpath "/home/u/.gnupg")
  (subpath "/home/u/.aws")
  (subpath "/home/u/.azure")
  (subpath "/home/u/.config/gcloud")
  (subpath "/home/u/.gcloud")
  (subpath "/home/u/.kube")
  (subpath "/home/u/.docker")
  (subpath "/home/u/.git-credentials")
  (subpath "/home/u/.netrc")
  (subpath "/home/u/.npmrc")
  (subpath "/home/u/.vault-token")
  (subpath "/home/u/.credentials")
  (subpath "/home/u/.secrets")
  (subpath "/home/u/.keys")
  (subpath "/home/u/.pki")
  (subpath "/home/u/.terraform.d")
  (subpath "/home/u/.config/op")
  (subpath "/home/u/.password-store")
  (subpath "/home/u/.1password")
  (subpath "/home/u/Library/Keychains")
  (subpath "/Library/Keychains")
  (subpath "/home/u/Library/Containers/com.1password.1password")
  (subpath "/home/u/Library/Group Containers/2BUA8C4S2C.com.1password")
  (subpath "/home/u/.zshrc")
  (subpath "/home/u/.zshenv")
  (subpath "/home/u/.zprofile")
  (subpath "/home/u/.zlogin")
  (subpath "/home/u/.zlogout")
  (subpath "/home/u/.bashrc")
  (subpath "/home/u/.bash_profile")
  (subpath "/home/u/.bash_login")
  (subpath "/home/u/.bash_logout")
  (subpath "/home/u/.profile")
  (subpath "/home/u/.config/fish")
  (subpath "/home/u/.env")
  (subpath "/home/u/.envrc")
  (subpath "/home/u/.bash_history")
  (subpath "/home/u/.zsh_history")
  (subpath "/home/u/.history")
  (subpath "/home/u/.python_history")
  (subpath "/home/u/Library/Application Support/1Password")
  (subpath "/home/u/Library/Application Support/Arc")
  (subpath "/home/u/Library/Application Support/BraveSoftware")
  (subpath "/home/u/Library/Application Support/Chromium")
  (subpath "/home/u/Library/Application Support/com.operasoftware.Opera")
  (subpath "/home/u/Library/Application Support/Firefox")
  (subpath "/home/u/Library/Application Support/Google/Chrome")
  (subpath "/home/u/Library/Application Support/Microsoft Edge")
  (subpath "/home/u/Library/Application Support/MobileSync")
  (subpath "/home/u/Library/Application Support/Vivaldi")
  (subpath "/home/u/Library/Safari")
  (subpath "/home/u/Library/Containers/com.apple.Safari")
  (subpath "/home/u/Library/Messages")
  (subpath "/home/u/Library/Mail")
  (subpath "/home/u/Library/Cookies")
  (subpath "/topsecret"))

;; confine writes to the workspace + a curated allowlist
(deny file-write*)
(allow file-write*
  (subpath "/work/ws")
  (subpath "/private/tmp")
  (subpath "/private/var/folders")
  (subpath "/tmp")
  (subpath "/home/u/.cache")
  (subpath "/home/u/.local/share")
  (subpath "/home/u/.local/state")
  (subpath "/home/u/Library/Caches")
  (subpath "/home/u/.cargo")
  (subpath "/home/u/.rustup")
  (subpath "/home/u/.npm")
  (subpath "/home/u/.node-gyp")
  (subpath "/home/u/.yarn")
  (subpath "/home/u/.pnpm-store")
  (subpath "/home/u/.deno")
  (subpath "/home/u/.bun")
  (subpath "/home/u/go")
  (subpath "/home/u/.gem")
  (subpath "/home/u/.bundle")
  (subpath "/home/u/.gradle")
  (subpath "/home/u/.m2")
  (subpath "/home/u/.ivy2")
  (subpath "/home/u/.sbt")
  (subpath "/home/u/.nuget")
  (subpath "/home/u/.dotnet")
  (subpath "/home/u/.cocoapods")
  (subpath "/extra")
  (literal "/dev/null") (literal "/dev/zero") (literal "/dev/random") (literal "/dev/urandom") (regex #"^/dev/tty") (regex #"^/dev/fd/") (regex #"^/dev/stdout"))
`;

Deno.test("buildProfile matches golden", () => {
  const profile = buildProfile({
    workspace: "/work/ws",
    home: "/home/u",
    allowWrite: ["/extra"],
    denyRead: ["/topsecret"],
  });
  assertEquals(profile, GOLDEN);
});

Deno.test("buildProfile: workspace is the first write-allowed path", () => {
  const p = buildProfile({ workspace: "/my/ws", home: "/home/u" });
  const allowIdx = p.indexOf("(allow file-write*");
  assertStringIncludes(p.slice(allowIdx), '(allow file-write*\n  (subpath "/my/ws")');
});

Deno.test("buildProfile: escapes quotes in paths", () => {
  const p = buildProfile({ workspace: '/w/a"b', home: "/home/u" });
  assertStringIncludes(p, '(subpath "/w/a\\"b")');
});

Deno.test("wrap prepends sandbox-exec -p <profile>", () => {
  const argv = wrap(["/bin/echo", "hi"], { workspace: "/w", home: "/home/u" });
  assertEquals(argv[0], "/usr/bin/sandbox-exec");
  assertEquals(argv[1], "-p");
  assertStringIncludes(argv[2], "(deny file-write*)");
  assertEquals(argv.slice(3), ["/bin/echo", "hi"]);
});

Deno.test("buildProfile: network is open by default (no confineNetwork)", () => {
  const p = buildProfile({ workspace: "/w", home: "/home/u" });
  assert(!p.includes("network"), "default profile must not restrict network");
});

Deno.test("buildProfile: confineNetwork denies egress but allows loopback", () => {
  const p = buildProfile({ workspace: "/w", home: "/home/u", confineNetwork: true });
  assertStringIncludes(p, "(deny network*)");
  assertStringIncludes(p, '(allow network-outbound (remote ip "localhost:*")');
  // loopback-only: the deny comes before the narrow allow, and no broad allow leaks
  assert(p.indexOf("(deny network*)") < p.indexOf("network-outbound"), "deny precedes allow");
});

// ---- real enforcement smokes (macOS only) ----------------------------------
// These shell out, so they need `--allow-run`. Under the current `deno task test`
// flags they self-skip; run them for real with `deno test --allow-run` (or the
// `--allow-run`-augmented task — see src/sandbox/INTEGRATION.md).

async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}

const isMac = Deno.build.os === "darwin";
const smokeOk = isMac && (await canRun("/usr/bin/sandbox-exec"));

async function runArgv(argv: string[]): Promise<number> {
  const { code } = await new Deno.Command(argv[0], {
    args: argv.slice(1),
    stdout: "null",
    stderr: "null",
  }).output();
  return code;
}

Deno.test({
  name: "seatbelt: write inside workspace succeeds",
  ignore: !smokeOk,
  fn: async () => {
    const ws = await Deno.makeTempDir();
    try {
      const argv = wrap(["/bin/sh", "-c", `echo ok > '${ws}/inside.txt'`], { workspace: ws });
      assertEquals(await runArgv(argv), 0);
      assertEquals((await Deno.readTextFile(`${ws}/inside.txt`)).trim(), "ok");
    } finally {
      await Deno.remove(ws, { recursive: true });
    }
  },
});

Deno.test({
  name: "seatbelt: write outside workspace is denied",
  ignore: !smokeOk,
  fn: async () => {
    const ws = await Deno.makeTempDir();
    // A sibling dir the profile does NOT allow (temp roots under /var are allowed,
    // so target a path outside both the workspace and any allowlisted prefix).
    const outside = `${Deno.env.get("HOME")}/.bough-seatbelt-should-not-write`;
    try {
      const argv = wrap(["/bin/sh", "-c", `echo leak > '${outside}'`], { workspace: ws });
      const code = await runArgv(argv);
      assert(code !== 0, "write outside workspace should fail");
      let created = false;
      try {
        await Deno.stat(outside);
        created = true;
      } catch { /* expected: not created */ }
      assert(!created, "file outside workspace must not exist");
    } finally {
      await Deno.remove(ws, { recursive: true });
      try {
        await Deno.remove(outside);
      } catch { /* not created, good */ }
    }
  },
});

Deno.test({
  name: "seatbelt: a workspace reached through a symlink is still writable",
  ignore: !smokeOk,
  fn: async () => {
    // Regression: Seatbelt matches canonicalized paths, so a workspace whose path
    // goes through a symlink would (without wrap()'s realPath) produce a rule that
    // never matches the kernel's resolved target — legit writes silently denied.
    // Build that case: a real dir under $HOME (outside every allowlisted prefix, so
    // the write can only succeed via the workspace rule) reached via a symlink.
    const home = Deno.env.get("HOME")!;
    const realWs = `${home}/.bough-symlink-ws-${crypto.randomUUID()}`;
    const linkWs = `${home}/.bough-symlink-link-${crypto.randomUUID()}`;
    await Deno.mkdir(realWs);
    await Deno.symlink(realWs, linkWs);
    try {
      // Use the symlinked path as the workspace; wrap() canonicalizes it to realWs.
      const argv = wrap(["/bin/sh", "-c", `echo ok > '${linkWs}/inside.txt'`], {
        workspace: linkWs,
      });
      assertEquals(await runArgv(argv), 0);
      assertEquals((await Deno.readTextFile(`${realWs}/inside.txt`)).trim(), "ok");
    } finally {
      await Deno.remove(linkWs).catch(() => {});
      await Deno.remove(realWs, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  name: "seatbelt: confineNetwork blocks direct egress but allows loopback (the bypass fix)",
  ignore: !smokeOk,
  fn: async () => {
    const ws = await Deno.makeTempDir();
    // A throwaway loopback listener stands in for the local proxy.
    const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
    const port = (listener.addr as Deno.NetAddr).port;
    (async () => {
      for await (const conn of listener) conn.close();
    })();
    try {
      // Direct public egress (the --noproxy bypass) must fail closed.
      const direct = wrap(
        ["/usr/bin/curl", "-sS", "-m", "6", "--noproxy", "*", "-o", "/dev/null", "http://example.com"],
        { workspace: ws, confineNetwork: true },
      );
      assert(await runArgv(direct) !== 0, "direct egress must be blocked under confineNetwork");

      // Loopback (the proxy) must still be reachable.
      const loop = wrap(
        ["/usr/bin/nc", "-z", "-G", "2", "127.0.0.1", String(port)],
        { workspace: ws, confineNetwork: true },
      );
      assertEquals(await runArgv(loop), 0);

      // Without confineNetwork, the same direct egress is allowed (opt-in posture).
      const open = wrap(
        ["/usr/bin/curl", "-sS", "-m", "8", "--noproxy", "*", "-o", "/dev/null", "http://example.com"],
        { workspace: ws },
      );
      assertEquals(await runArgv(open), 0);
    } finally {
      listener.close();
      await Deno.remove(ws, { recursive: true });
    }
  },
});
