/**
 * `bough mcp <verb>` — the headless client for the MCP registry.
 *
 * WHY THIS EXISTS. Everything here was already reachable: the routes have been in
 * `app.ts` since T7, and the `/mcp` panel drives all of them. But the panel is the
 * ONLY thing that did, which made the whole surface unusable from a script, from a
 * remote shell, and from an agent working on this repo — and unusable is where the
 * bugs hid. Diagnosing four broken servers meant hand-rolling `curl` against
 * `/mcp/servers/:name/connect`, reading a raw JSON error, and guessing at the verb
 * that would fix it. A CLI is how "is my MCP setup actually working" becomes a
 * question with a one-line answer.
 *
 * THE VERB THAT MATTERS IS `doctor`. `list` says what is registered and `test` says
 * whether one server answers, but the real question is never about one server — it
 * is "why is none of this working", and answering it means connecting everything and
 * saying, per server, which of the handful of distinct causes applies. Those causes
 * are knowable and few: not granted, no credential, a credential another client owns
 * that has gone stale, a credential that was never there, or an endpoint that
 * refuses. Each has a different fix and the errors alone do not sort them.
 *
 * Conventions are `cli/exec.ts`'s, for the same reasons stated there:
 *
 *   - **Argument parsing is pure and total.** `parseMcpArgs` is a function over a
 *     string array returning arguments or a usage error. It never reads the
 *     environment, never exits, never throws.
 *   - **Every effect is injected.** `runMcp` takes a `fetch`, two writers and an
 *     environment, and RETURNS an exit code. The `import.meta.main` block is the
 *     only code that touches a real process — which is what lets the whole client
 *     be tested against the real route table with nothing on the network.
 *
 * Exit codes are the contract with whatever wraps this:
 *
 *   0  the verb did what it says — including `doctor` finding everything healthy
 *   1  the operation ran and the answer is bad: a server did not connect, an
 *      authorization did not complete, `doctor` found something broken
 *   2  usage problem, or no server on the port
 *
 * The 0/1 split is what makes this usable in CI: `bough mcp doctor` exits non-zero
 * when the setup needs a human, and zero when it does not.
 */
import type { McpStatus } from "../mcp/status.ts";

/** Verbs, in the order the help lists them. */
const VERBS = [
  "list",
  "test",
  "auth",
  "logout",
  "grant",
  "revoke",
  "add",
  "remove",
  "doctor",
] as const;

export type McpVerb = (typeof VERBS)[number];

export interface McpArgs {
  verb: McpVerb;
  /** The server the verb acts on. Absent for `list` and `doctor`. */
  name?: string;
  /** `add` only: the remote endpoint. */
  url?: string;
  json: boolean;
  port?: number;
  /** `auth` only: seconds to wait for the browser round trip. */
  timeout: number;
  /**
   * `test`/`doctor` only: the conversation a LOCAL server's subprocess runs in.
   *
   * A stdio entry is a command spawned in a checkout, so the route refuses a
   * scopeless connect (`mcp/status.ts`) — there is no "the" workspace for a CLI.
   * Absent means local servers are reported as untested rather than as broken.
   */
  session?: string;
}

export interface McpUsageError {
  usageError: string;
}

export const USAGE = [
  "usage: bough mcp <verb> [name] [--json] [--port N]",
  "",
  "  list                    every server: grant, connection, credential",
  "  doctor                  connect them all and say what to do about each",
  "  test NAME               connect one server now and report its tools",
  "  auth NAME               authorize: prints a URL, waits, then connects",
  "  logout NAME             forget the credentials bough stored for NAME",
  "  grant NAME              let every conversation call it",
  "  revoke NAME             take that back, everywhere",
  "  add NAME URL            register a remote server",
  "  remove NAME             drop the registration and any grants it holds",
  "",
  "  --json                  machine-readable output",
  "  --session ID            test/doctor: conversation to run LOCAL servers in",
  "  --port N                server port (default BOUGH_PORT, else 4321)",
  "  --timeout SECS          auth only: how long to wait for the browser (default 180)",
  "",
  "exit: 0 fine · 1 something is broken · 2 usage or no server",
].join("\n");

/** Verbs that name a server. `add` needs a URL beside it. */
const NEEDS_NAME = new Set<McpVerb>([
  "test",
  "auth",
  "logout",
  "grant",
  "revoke",
  "add",
  "remove",
]);

function isVerb(s: string): s is McpVerb {
  return (VERBS as readonly string[]).includes(s);
}

export function isUsageError(x: McpArgs | McpUsageError): x is McpUsageError {
  return "usageError" in x;
}

/**
 * Parse `bough mcp`'s arguments. Pure and total.
 *
 * No verb at all is `list`, because the question people actually arrive with is
 * "what have I got" and making them type it is friction over the common case.
 */
export function parseMcpArgs(argv: readonly string[]): McpArgs | McpUsageError {
  const positional: string[] = [];
  let json = false;
  let port: number | undefined;
  let timeout = 180;
  let positionalSession: string | undefined;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "-h" || a === "--help") return { usageError: USAGE };
    if (a === "--json") {
      json = true;
      continue;
    }
    if (a === "--session") {
      const raw = argv[++i];
      if (raw === undefined) return { usageError: `--session needs a value\n${USAGE}` };
      positionalSession = raw;
      continue;
    }
    if (a === "--port" || a === "--timeout") {
      const raw = argv[++i];
      if (raw === undefined) return { usageError: `${a} needs a value\n${USAGE}` };
      const n = Number(raw);
      if (!Number.isFinite(n) || n <= 0) {
        return { usageError: `${a} needs a positive number, got "${raw}"\n${USAGE}` };
      }
      if (a === "--port") port = n;
      else timeout = n;
      continue;
    }
    if (a.startsWith("-")) return { usageError: `unknown flag ${a}\n${USAGE}` };
    positional.push(a);
  }
  const [verbRaw, name, url] = positional;
  if (verbRaw === undefined) {
    return {
      verb: "list",
      json,
      timeout,
      ...(port === undefined ? {} : { port }),
      ...(positionalSession === undefined ? {} : { session: positionalSession }),
    };
  }
  if (!isVerb(verbRaw)) {
    return { usageError: `unknown verb "${verbRaw}" — one of ${VERBS.join(", ")}\n${USAGE}` };
  }
  if (NEEDS_NAME.has(verbRaw) && !name) {
    return { usageError: `${verbRaw} needs a server name\n${USAGE}` };
  }
  if (verbRaw === "add" && !url) {
    return { usageError: `add needs a name and a URL: bough mcp add notion https://…\n${USAGE}` };
  }
  return {
    verb: verbRaw,
    json,
    timeout,
    ...(name === undefined ? {} : { name }),
    ...(positionalSession === undefined ? {} : { session: positionalSession }),
    ...(url === undefined ? {} : { url }),
    ...(port === undefined ? {} : { port }),
  };
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

export interface McpDeps {
  fetch: typeof fetch;
  out: (line: string) => void;
  err: (line: string) => void;
  env: Record<string, string | undefined>;
  /** Injected so the auth poll does not sleep in tests. */
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

/** One server's connect outcome, as the route reports it. */
interface ConnectResult {
  server: string;
  connected: boolean;
  error?: string;
  tools?: { name: string }[];
}

/** How a row reads. The glyphs are the panel's, deliberately — one vocabulary. */
function glyph(status: McpStatus, name: string): string {
  if (status.connections.find((c) => c.server === name)?.alive) return "●";
  return status.active.includes(name) ? "◐" : "○";
}

/**
 * Every distinct reason a server is not usable, and what to do about it.
 *
 * THIS IS THE POINT OF `doctor`. The connect error alone does not sort these: "has
 * no string at #mcpOAuth…" and "expired at …" and a 401 are three different jobs for
 * the user, and two of them are not bough's to fix. Ordered by what has to be true
 * first — a server nobody granted will never connect, so saying anything about its
 * credential would be advice about a step that has not been reached.
 */
function diagnose(
  status: McpStatus,
  name: string,
  conn: ConnectResult | null,
  session?: string,
): { state: "ok" | "bad" | "unknown"; note: string } {
  if (conn?.connected) {
    const n = conn.tools?.length ?? 0;
    return { state: "ok", note: `${n} tool${n === 1 ? "" : "s"}` };
  }
  if (!status.active.includes(name)) {
    return { state: "bad", note: `not granted — bough mcp grant ${name}` };
  }
  // LOCAL SERVERS CANNOT BE TESTED WITHOUT A CONVERSATION, and that is not a fault.
  // A stdio entry is a command spawned in a checkout, so the route refuses a
  // scopeless connect — there is no "the" workspace for a CLI to pick. Reported as
  // UNKNOWN rather than broken: counting an untested server as a failure would make
  // `doctor` exit 1 on a perfectly good setup, and the exit code is the part of this
  // verb a script depends on.
  const remote = typeof status.registry.servers[name]?.url === "string";
  if (!remote && !session) {
    return {
      state: "unknown",
      note: `local command — not tested; needs a conversation: bough mcp doctor --session ID`,
    };
  }
  const error = conn?.error ?? "did not connect";
  // A credential this machine's OTHER client owns. bough deliberately never
  // refreshes one it did not obtain (`mcp/keychain.ts`), so the fix is always in
  // that client and saying so beats repeating the error.
  if (/expired at/.test(error)) {
    return {
      state: "bad",
      note: `its Claude Code grant expired — use that server in Claude Code once, ` +
        `or: bough mcp auth ${name}`,
    };
  }
  if (/has no string at/.test(error)) {
    return {
      state: "bad",
      note: `Claude Code's grant for it is empty — re-authorize it there, or ` +
        `authorize bough separately: bough mcp auth ${name}`,
    };
  }
  // ONLY REMOTE SERVERS HAVE CREDENTIALS. `status.auth` is populated for `url`
  // entries alone, so a local command always reads as unauthorized — and telling
  // someone to run `bough mcp auth` on a stdio server sends them to a flow that
  // cannot exist. This was live for exactly one commit and `doctor` said it about
  // both of the local servers on the machine it was written for.
  if (remote && !status.auth[name]?.authorized) {
    return { state: "bad", note: `no credential — bough mcp auth ${name}` };
  }
  return { state: "bad", note: error };
}

function base(args: McpArgs, env: Record<string, string | undefined>): string {
  const port = args.port ?? Number(env["BOUGH_PORT"] ?? 4321);
  return `http://127.0.0.1:${port}`;
}

/** A request, with the server-is-not-running case turned into exit code 2. */
async function call(
  deps: McpDeps,
  url: string,
  init?: RequestInit,
): Promise<{ status: number; body: any } | null> {
  try {
    const res = await deps.fetch(url, init);
    const text = await res.text();
    let body: unknown = null;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {
      body = { error: text };
    }
    return { status: res.status, body };
  } catch (e) {
    deps.err(
      `no bough server at ${new URL(url).host} (${
        e instanceof Error ? e.message : String(e)
      }). Start one: bough start`,
    );
    return null;
  }
}

/** The error sentence a route returned, or a fallback naming the status. */
function errorOf(r: { status: number; body: any }): string {
  return typeof r.body?.error === "string" ? r.body.error : `HTTP ${r.status}`;
}

export async function runMcp(argv: readonly string[], deps: McpDeps): Promise<number> {
  const parsed = parseMcpArgs(argv);
  if (isUsageError(parsed)) {
    deps.err(parsed.usageError);
    return 2;
  }
  const args = parsed;
  const root = base(args, deps.env);
  const sleep = deps.sleep ?? ((ms: number) => new Promise((r) => setTimeout(r, ms)));

  const status = async (): Promise<McpStatus | null> => {
    const r = await call(deps, `${root}/mcp/servers`);
    if (!r) return null;
    if (r.status !== 200) {
      deps.err(errorOf(r));
      return null;
    }
    return r.body as McpStatus;
  };

  const connect = async (name: string, session?: string): Promise<ConnectResult | null> => {
    const q = session ? `?session=${encodeURIComponent(session)}` : "";
    const r = await call(deps, `${root}/mcp/servers/${encodeURIComponent(name)}/connect${q}`, {
      method: "POST",
    });
    if (!r) return null;
    // A route-level refusal (an unknown name) is not a connect result; report it as
    // one so every caller has a single shape to read.
    if (r.status >= 400 && r.body?.connected === undefined) {
      return { server: name, connected: false, error: errorOf(r) };
    }
    return r.body as ConnectResult;
  };

  switch (args.verb) {
    case "list": {
      const s = await status();
      if (!s) return 2;
      const names = Object.keys(s.registry.servers).sort();
      if (args.json) {
        deps.out(JSON.stringify(s, null, 2));
        return 0;
      }
      if (names.length === 0) {
        deps.out("no MCP servers registered — bough mcp add NAME URL, or bough sync-mcp");
        return 0;
      }
      for (const name of names) {
        const conn = s.connections.find((c) => c.server === name);
        const bits = [
          s.active.includes(name) ? "granted" : "not granted",
          conn?.alive ? `${conn.toolCount} tools` : null,
          s.auth[name]?.authorized ? "authed" : null,
          conn?.error ?? null,
        ].filter(Boolean);
        deps.out(`${glyph(s, name)} ${name}  ${bits.join(" · ")}`);
      }
      // The glyph legend, for the same reason the panel grew one: three marks
      // carrying the whole state of a row, explained nowhere, is how "it stays a
      // half circle" becomes a bug report.
      deps.out("");
      deps.out("● connected · ◐ granted, not connected · ○ not granted");
      return 0;
    }

    case "doctor": {
      const s = await status();
      if (!s) return 2;
      const names = Object.keys(s.registry.servers).sort();
      if (names.length === 0) {
        deps.out("no MCP servers registered — bough mcp add NAME URL, or bough sync-mcp");
        return 0;
      }
      // Sequential ON PURPOSE. These connect to third-party endpoints and some of
      // them spawn subprocesses; a burst of parallel handshakes makes a slow server
      // look like a broken one, and the output is read top to bottom anyway.
      const rows: { name: string; state: "ok" | "bad" | "unknown"; note: string }[] = [];
      for (const name of names) {
        const remote = typeof s.registry.servers[name]?.url === "string";
        // Do not spawn a connect that is already known to be refused: a local server
        // with no session gets its answer from `diagnose` without a round trip.
        const testable = s.active.includes(name) && (remote || !!args.session);
        const conn = testable ? await connect(name, args.session) : null;
        rows.push({ name, ...diagnose(s, name, conn, args.session) });
      }
      if (args.json) {
        deps.out(JSON.stringify(rows, null, 2));
      } else {
        const mark = { ok: "✓", bad: "✗", unknown: "?" } as const;
        for (const r of rows) deps.out(`${mark[r.state]} ${r.name}  ${r.note}`);
        const bad = rows.filter((r) => r.state === "bad").length;
        const unknown = rows.filter((r) => r.state === "unknown").length;
        deps.out("");
        deps.out(
          bad === 0
            ? `all ${rows.length - unknown} tested server${
              rows.length - unknown === 1 ? "" : "s"
            } working` + (unknown > 0 ? ` · ${unknown} not tested` : "")
            : `${bad} of ${rows.length} need${bad === 1 ? "s" : ""} attention` +
              (unknown > 0 ? ` · ${unknown} not tested` : ""),
        );
      }
      return rows.some((r) => r.state === "bad") ? 1 : 0;
    }

    case "test": {
      const r = await connect(args.name!, args.session);
      if (!r) return 2;
      if (args.json) {
        deps.out(JSON.stringify(r, null, 2));
        return r.connected ? 0 : 1;
      }
      if (r.connected) {
        const tools = r.tools ?? [];
        deps.out(
          `✓ ${args.name} connected · ${tools.length} tool${tools.length === 1 ? "" : "s"}` +
            (tools.length > 0 ? `\n  ${tools.map((t) => t.name).join(", ")}` : ""),
        );
        return 0;
      }
      deps.err(`✗ ${args.name} did not connect — ${r.error ?? "no reason given"}`);
      return 1;
    }

    case "auth": {
      const name = args.name!;
      const begun = await call(deps, `${root}/mcp/servers/${encodeURIComponent(name)}/auth`, {
        method: "POST",
      });
      if (!begun) return 2;
      if (begun.status >= 400) {
        deps.err(errorOf(begun));
        return 1;
      }
      if (begun.body?.status === "authorized") {
        deps.out(`${name} was already authorized`);
      } else {
        const url = begun.body?.authorizationUrl;
        if (!url) {
          deps.err(`${name}: the server asked for authorization but sent no URL`);
          return 1;
        }
        if (begun.body?.correctedUrl) {
          // The registry was rewritten on the way through (`mcp/oauth.ts`): the
          // published endpoint is often not the one the flow wants, and a silent
          // rewrite is a surprise the next reader of the registry has to solve.
          deps.out(`note: its endpoint was corrected to ${begun.body.correctedUrl}`);
        }
        // PRINTED, never opened. This client is used over SSH and in CI as often as
        // on a desktop, and shelling out to a browser hangs where there is none.
        deps.out(`open this to authorize ${name}, then come back — it finishes on its own:`);
        deps.out(`  ${url}`);
        const deadline = (deps.now ?? Date.now)() + args.timeout * 1000;
        let authorized = false;
        while ((deps.now ?? Date.now)() < deadline) {
          await sleep(1000);
          const st = await call(deps, `${root}/mcp/servers/${encodeURIComponent(name)}/auth`);
          if (!st) return 2;
          if (st.body?.authorized) {
            authorized = true;
            break;
          }
        }
        if (!authorized) {
          deps.err(`${name}: still waiting on the browser after ${args.timeout}s — run auth again`);
          return 1;
        }
        deps.out(`${name} is authorized`);
      }
      // CONNECT, do not stop at "authorized". Storing tokens changes no observable
      // state — the panel's `◐` is about a CONNECTION — and a flow whose success is
      // invisible reads as a flow that failed.
      const r = await connect(name);
      if (!r) return 2;
      if (r.connected) {
        const n = r.tools?.length ?? 0;
        deps.out(`✓ ${name} connected · ${n} tool${n === 1 ? "" : "s"}`);
        if (!(await status())?.active.includes(name)) {
          deps.out(`  not granted yet — bough mcp grant ${name}`);
        }
        return 0;
      }
      deps.err(`${name} is authorized but did not connect — ${r.error ?? "no reason given"}`);
      return 1;
    }

    case "logout": {
      const r = await call(deps, `${root}/mcp/servers/${encodeURIComponent(args.name!)}/auth`, {
        method: "DELETE",
      });
      if (!r) return 2;
      if (r.status >= 400) {
        deps.err(errorOf(r));
        return 1;
      }
      deps.out(`forgot bough's credentials for ${args.name} — the registration is untouched`);
      return 0;
    }

    case "grant":
    case "revoke": {
      const on = args.verb === "grant";
      const r = await call(
        deps,
        `${root}/mcp/servers/${encodeURIComponent(args.name!)}/${on ? "enable" : "disable"}`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          // The GLOBAL scope: every conversation. A per-session grant is a thing the
          // panel does because it has a session on screen; a CLI does not, and
          // inventing one here would make the verb mean something different from
          // what it says.
          body: JSON.stringify({ sessionId: "" }),
        },
      );
      if (!r) return 2;
      if (r.status >= 400) {
        deps.err(errorOf(r));
        return 1;
      }
      deps.out(
        on
          ? `${args.name} is granted in every conversation`
          : `${args.name} is revoked everywhere`,
      );
      return 0;
    }

    case "add": {
      const r = await call(deps, `${root}/mcp/servers/${encodeURIComponent(args.name!)}`, {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ url: args.url }),
      });
      if (!r) return 2;
      if (r.status >= 400) {
        deps.err(errorOf(r));
        return 1;
      }
      deps.out(`${args.name} registered — bough mcp auth ${args.name}, then grant it`);
      return 0;
    }

    case "remove": {
      const r = await call(deps, `${root}/mcp/servers/${encodeURIComponent(args.name!)}`, {
        method: "DELETE",
      });
      if (!r) return 2;
      if (r.status >= 400) {
        deps.err(errorOf(r));
        return 1;
      }
      deps.out(`${args.name} removed, along with any grants it held`);
      return 0;
    }
  }
}

if (import.meta.main) {
  const code = await runMcp(process.argv.slice(2), {
    fetch: globalThis.fetch,
    out: (l) => console.log(l),
    err: (l) => console.error(l),
    env: process.env,
  });
  process.exit(code);
}
