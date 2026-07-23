/**
 * Store gateway — git smart-HTTP (v0 stateless-rpc) serving each session's
 * shadow store to its guest VM, bound on the gate host IP (the one address a
 * locked-down guest can reach; vm.ts --allow-cidr). Both directions:
 *
 *   - upload-pack (guest fetch/clone) runs HOST-side, so the store's
 *     objects/info/alternates → origin objects resolve natively and the grafted
 *     origin history transfers — a mounted-store clone can never see it.
 *   - receive-pack (guest push = snapshot) runs HOST-side against the
 *     authoritative filesystem, serialized with the host's own ref writers via
 *     shadow.withLock — the virtiofs refs-before-objects race that failed 13/15
 *     concurrent mounted pushes is structurally gone.
 *
 * Auth: per-session bearer token (minted at ensureVm, stamped into the guest
 * clone's http.extraHeader), constant-time compared. Receive is confined to the
 * session's own ref by the store's pre-receive hook via BOUGH_RECEIVE_REF
 * (written by ensureStore). After a receive that moved the session ref, the
 * read-only mirror is refreshed and `changes.updated` is published on the
 * app-wide bus so the Changes rail refetches.
 *
 * Deliberately NOT behind the Claw Patrol proxy: pushing snapshots to bough's
 * own store is the snapshot mechanism, not egress (the review rail stays the
 * only path to the origin). NO_PROXY gains the gate IP in envFor.
 */
import { bus } from "../bus.ts";
import { gateHostIp } from "../sandbox/gatehost.ts";
import { refFor, storeForSession, withLock } from "./shadow.ts";
import { refreshMirror } from "./mirror.ts";

/** Spawned-git env: user/system git config must not leak (mirrors shadow.ts). */
const ISOLATED = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };

const SERVICES = ["git-upload-pack", "git-receive-pack"] as const;
type Service = (typeof SERVICES)[number];

let server: Deno.HttpServer<Deno.NetAddr> | null = null;
const tokens = new Map<string, string>(); // sessionId → bearer token

/** Start the gateway (idempotent): random port on the gate host IP, held for
 * the server lifetime. Sessions re-stamp their remote URL on reattach because
 * the port differs across server runs. */
export function startGitGateway(): void {
  if (server) return;
  server = Deno.serve(
    {
      hostname: gateHostIp(),
      port: 0,
      onListen: ({ hostname, port }) => console.log(`git gateway on ${hostname}:${port}`),
    },
    handle,
  );
}

/** Stop and forget the gateway (tests). */
export async function stopGitGateway(): Promise<void> {
  const s = server;
  server = null;
  await s?.shutdown();
}

/** The session's clone/fetch/push URL as the guest reaches it. Throws unstarted. */
export function gitGatewayUrl(sessionId: string): string {
  if (!server) throw new Error("git gateway not started");
  return `http://${server.addr.hostname}:${server.addr.port}/git/${sessionId}`;
}

/** Mint (or rotate) the session's bearer token. Stamped into the guest clone's
 * http.extraHeader by the VM bootstrap; never persisted. */
export function mintSessionToken(sessionId: string): string {
  const token = crypto.randomUUID();
  tokens.set(sessionId, token);
  return token;
}

export function revokeSessionToken(sessionId: string): void {
  tokens.delete(sessionId);
}

/** Constant-time bearer compare: fixed-width digests, no length leak. */
async function tokenMatches(given: string, expected: string): Promise<boolean> {
  const digest = async (s: string) =>
    new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s)));
  const [a, b] = [await digest(given), await digest(expected)];
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

function pktLine(s: string): string {
  return (s.length + 4).toString(16).padStart(4, "0") + s;
}

/** The request body, gunzipped when the client compressed it (git does for big packs). */
function bodyStream(req: Request): ReadableStream<Uint8Array> {
  const body = req.body ?? new ReadableStream<Uint8Array>({ start: (c) => c.close() });
  return req.headers.get("content-encoding") === "gzip"
    ? body.pipeThrough(new DecompressionStream("gzip") as ReadableWritablePair<Uint8Array>)
    : body;
}

async function refShaIn(store: string, ref: string): Promise<string | null> {
  const r = await new Deno.Command("git", {
    args: [`--git-dir=${store}`, "rev-parse", "--verify", "-q", ref],
    env: ISOLATED,
    stdout: "piped",
    stderr: "null",
  }).output();
  return r.code === 0 ? new TextDecoder().decode(r.stdout).trim() : null;
}

async function handle(req: Request): Promise<Response> {
  const url = new URL(req.url);
  const m = url.pathname.match(/^\/git\/([^/]+)\/(info\/refs|git-upload-pack|git-receive-pack)$/);
  if (!m) return new Response("not found", { status: 404 });
  const [, sessionId, tail] = m;
  const expected = tokens.get(sessionId);
  const given = req.headers.get("authorization")?.match(/^Bearer (.+)$/)?.[1];
  if (!expected || !given || !(await tokenMatches(given, expected))) {
    return new Response("unauthorized", { status: 401 });
  }
  let store: string;
  try {
    store = await storeForSession(sessionId);
  } catch {
    return new Response("unknown session", { status: 404 });
  }
  if (tail === "info/refs") {
    const service = url.searchParams.get("service") as Service | null;
    if (req.method !== "GET" || !service || !SERVICES.includes(service)) {
      return new Response("smart-HTTP only", { status: 400 });
    }
    return await advertise(store, service);
  }
  const service = tail as Service;
  if (req.method !== "POST") return new Response("method not allowed", { status: 405 });
  return service === "git-upload-pack"
    ? uploadPack(store, req)
    : await receivePack(store, sessionId, req);
}

/** GET info/refs: the service's ref advertisement, prefixed per smart-HTTP v0. */
async function advertise(store: string, service: Service): Promise<Response> {
  const child = new Deno.Command("git", {
    args: [service.slice(4), "--stateless-rpc", "--advertise-refs", store],
    env: ISOLATED,
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const out = await child.output();
  if (out.code !== 0) {
    return new Response(new TextDecoder().decode(out.stderr), { status: 500 });
  }
  const header = new TextEncoder().encode(pktLine(`# service=${service}\n`) + "0000");
  const body = new Uint8Array(header.length + out.stdout.length);
  body.set(header);
  body.set(out.stdout, header.length);
  return new Response(body, {
    headers: {
      "content-type": `application/x-${service}-advertisement`,
      "cache-control": "no-cache",
    },
  });
}

/** POST git-upload-pack: fetch/clone. Streams the pack — no lock needed, git
 * reads a consistent snapshot of objects it has already resolved refs for. */
function uploadPack(store: string, req: Request): Response {
  const child = new Deno.Command("git", {
    args: ["upload-pack", "--stateless-rpc", store],
    env: ISOLATED,
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  bodyStream(req).pipeTo(child.stdin).catch(() => {});
  child.stderr.cancel().catch(() => {});
  child.status.then((s) => {
    if (s.code !== 0) console.error(`git gateway: upload-pack exited ${s.code}`);
  });
  return new Response(child.stdout, {
    headers: {
      "content-type": "application/x-git-upload-pack-result",
      "cache-control": "no-cache",
    },
  });
}

/** POST git-receive-pack: a guest snapshot push. Fully buffered inside
 * withLock(store) so it serializes with host accept/adopt ref writers; the
 * pre-receive hook (BOUGH_RECEIVE_REF) confines it to the session's own ref. */
async function receivePack(store: string, sessionId: string, req: Request): Promise<Response> {
  const body = new Uint8Array(await new Response(bodyStream(req)).arrayBuffer());
  const ref = refFor(sessionId);
  return await withLock(store, async () => {
    const before = await refShaIn(store, ref);
    const child = new Deno.Command("git", {
      args: ["receive-pack", "--stateless-rpc", store],
      env: { ...ISOLATED, BOUGH_RECEIVE_REF: ref },
      stdin: "piped",
      stdout: "piped",
      stderr: "piped",
    }).spawn();
    const w = child.stdin.getWriter();
    await w.write(body);
    await w.close();
    const out = await child.output();
    if (out.code !== 0) {
      return new Response(new TextDecoder().decode(out.stderr), { status: 500 });
    }
    const after = await refShaIn(store, ref);
    if (after && after !== before) {
      // The session tip moved: refresh the read-side mirror, wake the rail.
      await refreshMirror(sessionId).catch((e) =>
        console.error(`git gateway: mirror refresh failed for ${sessionId}: ${e.message}`)
      );
      bus.publish({ type: "changes.updated", sessionId, data: { sessionId } });
    }
    return new Response(out.stdout, {
      headers: {
        "content-type": "application/x-git-receive-pack-result",
        "cache-control": "no-cache",
      },
    });
  });
}
