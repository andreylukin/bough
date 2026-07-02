/**
 * macOS Seatbelt (`sandbox-exec`) profile generation for bough's filesystem
 * sandbox. Every shelled subprocess runs wrapped in this profile: reads are
 * allow-default MINUS a curated credential/secret denylist, and writes are
 * deny-default EXCEPT the workspace plus a curated toolchain allowlist. Ported
 * from the Gleam `seatbelt.gleam` (packages/bough_server) — same deny/allow groups.
 *
 * Scope is filesystem + process only. Network egress is intentionally NOT
 * restricted here; that is owned by the Claw Patrol layer (see docs). Kept pure
 * and dependency-light: `buildProfile` is a deterministic string function that
 * golden-tests, `wrap` just prepends the `sandbox-exec` argv.
 *
 * macOS-only. `sandbox-exec` is deprecated-but-present on current macOS and is
 * the same primitive the old sandbox and Chromium rely on.
 */

const SANDBOX_EXEC = "/usr/bin/sandbox-exec";

/**
 * Credential / secret / private paths denied for reading. Ported verbatim from
 * the Gleam profile's locked deny groups. `~` expands to `home` at build time;
 * `/`-rooted entries are absolute.
 */
const DENY_READS = [
  // credentials
  "~/.ssh",
  "~/.gnupg",
  "~/.aws",
  "~/.azure",
  "~/.config/gcloud",
  "~/.gcloud",
  "~/.kube",
  "~/.docker",
  "~/.git-credentials",
  "~/.netrc",
  "~/.npmrc",
  "~/.vault-token",
  "~/.credentials",
  "~/.secrets",
  "~/.keys",
  "~/.pki",
  "~/.terraform.d",
  "~/.config/op",
  "~/.password-store",
  "~/.1password",
  // keychains / password stores
  "~/Library/Keychains",
  "/Library/Keychains",
  "~/Library/Containers/com.1password.1password",
  "~/Library/Group Containers/2BUA8C4S2C.com.1password",
  // shell configs (may embed secrets) + history
  "~/.zshrc",
  "~/.zshenv",
  "~/.zprofile",
  "~/.zlogin",
  "~/.zlogout",
  "~/.bashrc",
  "~/.bash_profile",
  "~/.bash_login",
  "~/.bash_logout",
  "~/.profile",
  "~/.config/fish",
  "~/.env",
  "~/.envrc",
  "~/.bash_history",
  "~/.zsh_history",
  "~/.history",
  "~/.python_history",
  // browser data
  "~/Library/Application Support/1Password",
  "~/Library/Application Support/Arc",
  "~/Library/Application Support/BraveSoftware",
  "~/Library/Application Support/Chromium",
  "~/Library/Application Support/com.operasoftware.Opera",
  "~/Library/Application Support/Firefox",
  "~/Library/Application Support/Google/Chrome",
  "~/Library/Application Support/Microsoft Edge",
  "~/Library/Application Support/MobileSync",
  "~/Library/Application Support/Vivaldi",
  "~/Library/Safari",
  "~/Library/Containers/com.apple.Safari",
  // macOS private data
  "~/Library/Messages",
  "~/Library/Mail",
  "~/Library/Cookies",
];

/**
 * Dirs outside the workspace that toolchains legitimately write to (caches, temp,
 * per-language stores). Without these, cargo/npm/go/etc. break under
 * write-confinement. Ported from the Gleam allowlist.
 */
const WRITE_ALLOW = [
  // temp
  "/private/tmp",
  "/private/var/folders",
  "/tmp",
  // XDG + generic caches
  "~/.cache",
  "~/.local/share",
  "~/.local/state",
  "~/Library/Caches",
  // rust / node / python / go / ruby / java / .net toolchains
  "~/.cargo",
  "~/.rustup",
  "~/.npm",
  "~/.node-gyp",
  "~/.yarn",
  "~/.pnpm-store",
  "~/.deno",
  "~/.bun",
  "~/go",
  "~/.gem",
  "~/.bundle",
  "~/.gradle",
  "~/.m2",
  "~/.ivy2",
  "~/.sbt",
  "~/.nuget",
  "~/.dotnet",
  "~/.cocoapods",
];

/** Device files processes need to write (null sink, ptys, pipes). */
const DEV_WRITES = [
  '(literal "/dev/null")',
  '(literal "/dev/zero")',
  '(literal "/dev/random")',
  '(literal "/dev/urandom")',
  '(regex #"^/dev/tty")',
  '(regex #"^/dev/fd/")',
  '(regex #"^/dev/stdout")',
].join(" ");

export interface SandboxOptions {
  /** The read-write root (the session workspace). Required. */
  workspace: string;
  /** Home dir for `~` expansion. Defaults to `$HOME`. */
  home?: string;
  /** Extra write-allowed dirs beyond the toolchain allowlist (e.g. a snapshot dir). */
  allowWrite?: string[];
  /** Extra read-denied paths beyond the credential denylist. */
  denyRead?: string[];
}

function resolveHome(home?: string): string {
  const h = home ?? Deno.env.get("HOME");
  if (!h) throw new Error("seatbelt: no home dir (pass opts.home or set $HOME)");
  return h;
}

function expand(p: string, home: string): string {
  return p.startsWith("~") ? home + p.slice(1) : p;
}

function subpath(p: string): string {
  // Escape backslashes and quotes so a path with either can't break the SBPL string.
  const esc = p.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  return `(subpath "${esc}")`;
}

/**
 * The Seatbelt profile text (SBPL). Deterministic given `workspace`, `home`, and
 * the extra lists — golden-tested. Pure: no env reads (pass `home` explicitly).
 */
export function buildProfile(opts: SandboxOptions & { home: string }): string {
  const { workspace, home, allowWrite = [], denyRead = [] } = opts;

  const denies = [...DENY_READS, ...denyRead]
    .map((p) => subpath(expand(p, home)))
    .join("\n  ");

  const allows = [workspace, ...WRITE_ALLOW, ...allowWrite]
    .map((p) => subpath(expand(p, home)))
    .join("\n  ");

  return [
    "(version 1)",
    "(allow default)",
    "",
    ";; deny reads of credential/secret/private paths (ported from the Gleam sandbox)",
    `(deny file-read*\n  ${denies})`,
    "",
    ";; confine writes to the workspace + a curated allowlist",
    "(deny file-write*)",
    `(allow file-write*\n  ${allows}\n  ${DEV_WRITES})`,
    "",
  ].join("\n");
}

/** Canonical path (resolving symlinks), or the input unchanged if it doesn't exist. */
function canonical(p: string): string {
  try {
    return Deno.realPathSync(p);
  } catch {
    return p;
  }
}

/**
 * Build the `sandbox-exec` argv that runs `cmd` under the generated profile. The
 * profile travels inline via `-p`, so no temp file is written and nothing races
 * between concurrent sessions. `cmd` is the full argv of the target program
 * (e.g. `["/bin/sh", "-c", "..."]`).
 *
 * Seatbelt matches the kernel's canonicalized paths, so a workspace/allow/deny
 * path that goes through a symlink (e.g. `/tmp` → `/private/tmp`, `/var` →
 * `/private/var`) must be resolved here or its rule silently won't match. We
 * `realPathSync` existing paths; a non-existent `denyRead` entry passes through
 * unchanged (a path that doesn't exist can't be read anyway).
 */
export function wrap(cmd: string[], opts: SandboxOptions): string[] {
  const home = resolveHome(opts.home);
  const profile = buildProfile({
    ...opts,
    home,
    workspace: canonical(opts.workspace),
    allowWrite: opts.allowWrite?.map(canonical),
    denyRead: opts.denyRead?.map(canonical),
  });
  return [SANDBOX_EXEC, "-p", profile, ...cmd];
}
