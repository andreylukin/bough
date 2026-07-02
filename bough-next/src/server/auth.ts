/**
 * Opt-in password gate for remote access (e.g. serving through a Cloudflare tunnel).
 * When a password is configured (BOUGH_PASSWORD → AppCtx.password) every request must
 * carry a session cookie minted by POST /auth/login; without one, API calls get a 401
 * JSON error and browser navigations get a small login page. With no password set,
 * the gate is a no-op and local use stays frictionless.
 *
 * Sessions are random tokens held in memory — a server restart logs everyone out,
 * which is fine for a single-user tool. The cookie is HttpOnly + SameSite=Lax, and
 * EventSource sends cookies on same-origin requests, so /events works unchanged.
 * Failed logins are delayed to blunt online brute force.
 */

const COOKIE = "bough_session";
const MAX_AGE = 60 * 60 * 24 * 30; // 30 days
const FAIL_DELAY_MS = 300;

/** Constant-time string comparison (length still leaks; passwords vary anyway). */
function safeEqual(a: string, b: string): boolean {
  const ea = new TextEncoder().encode(a);
  const eb = new TextEncoder().encode(b);
  if (ea.length !== eb.length) return false;
  let diff = 0;
  for (let i = 0; i < ea.length; i++) diff |= ea[i] ^ eb[i];
  return diff === 0;
}

function cookieToken(req: Request): string | undefined {
  const header = req.headers.get("cookie");
  if (!header) return undefined;
  for (const pair of header.split(";")) {
    const eq = pair.indexOf("=");
    if (eq < 0) continue;
    if (pair.slice(0, eq).trim() === COOKIE) return pair.slice(eq + 1).trim();
  }
  return undefined;
}

// Styled to match the placeholder page in static.ts.
const loginPage = (error: boolean) =>
  `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>bough — sign in</title><style>body{font:15px/1.6 system-ui,sans-serif;background:#15171a;
color:#d8dde3;display:grid;place-items:center;height:100vh;margin:0}
input{font:inherit;background:#1e2126;color:#d8dde3;border:1px solid #3a3f46;border-radius:6px;
padding:8px 10px}button{font:inherit;background:#7ec699;color:#15171a;border:0;border-radius:6px;
padding:8px 14px;cursor:pointer}.err{color:#e07878}</style></head><body>
<form method="post" action="/auth/login"><h1>bough</h1>
${error ? '<p class="err">Wrong password.</p>' : ""}
<p><input type="password" name="password" placeholder="password" autofocus autocomplete="current-password">
<button type="submit">Sign in</button></p></form></body></html>`;

function loginPageResponse(error: boolean): Response {
  return new Response(loginPage(error), {
    status: 401,
    headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" },
  });
}

export interface Auth {
  /**
   * Gate a request. Returns null when it may proceed to the router (no password
   * configured, valid session cookie, or CORS preflight handled elsewhere); otherwise
   * the response to send (login page, 401, or the /auth/login result).
   */
  gate(req: Request): Promise<Response | null>;
}

export function createAuth(password: string | undefined): Auth {
  const sessions = new Set<string>();

  async function login(req: Request): Promise<Response> {
    // Accept the login form (urlencoded) or JSON {password} — same field either way.
    const type = req.headers.get("content-type") ?? "";
    let supplied = "";
    if (type.includes("application/json")) {
      const body = await req.json().catch(() => null);
      if (body && typeof body === "object" && typeof (body as { password?: unknown }).password === "string") {
        supplied = (body as { password: string }).password;
      }
    } else {
      const form = await req.formData().catch(() => null);
      const value = form?.get("password");
      if (typeof value === "string") supplied = value;
    }

    if (!safeEqual(supplied, password!)) {
      await new Promise((r) => setTimeout(r, FAIL_DELAY_MS));
      return type.includes("application/json")
        ? new Response(JSON.stringify({ error: "wrong password" }), {
          status: 401,
          headers: { "content-type": "application/json" },
        })
        : loginPageResponse(true);
    }

    const token = crypto.randomUUID();
    sessions.add(token);
    const cookie = `${COOKIE}=${token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${MAX_AGE}`;
    return type.includes("application/json")
      ? new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "content-type": "application/json", "set-cookie": cookie },
      })
      // Form login: bounce back to the app root, cookie in hand.
      : new Response(null, { status: 303, headers: { location: "/", "set-cookie": cookie } });
  }

  return {
    // deno-lint-ignore require-await
    async gate(req) {
      if (!password) return null;
      const { pathname } = new URL(req.url);
      if (req.method === "POST" && pathname === "/auth/login") return login(req);
      const token = cookieToken(req);
      if (token && sessions.has(token)) return null;
      // Browser navigation → the login page; anything else (API, assets) → 401 JSON.
      if (req.method === "GET" && (req.headers.get("accept") ?? "").includes("text/html")) {
        return loginPageResponse(false);
      }
      return new Response(JSON.stringify({ error: "unauthorized" }), {
        status: 401,
        headers: { "content-type": "application/json" },
      });
    },
  };
}
