/**
 * OAuth for remote MCP servers (mcp.notion.com and friends): the SDK's auth()
 * drives discovery + dynamic client registration + PKCE; this module supplies the
 * OAuthClientProvider it needs and owns persistence and flow state.
 *
 * bough is a PUBLIC client (token_endpoint_auth_method "none", PKCE) and hosts the
 * redirect itself: the authorization server sends the user's browser back to
 * GET /mcp/oauth/callback on bough's own port — no shim, no extra listener. The
 * provider never redirects anything: beginAuth() CAPTURES the authorization URL
 * and hands it to the human (via the /mcp skill or the API response); the browser
 * half of the flow is theirs.
 *
 * Everything persists per server under ~/.bough/mcp/tokens/<server>.json (0600):
 * dynamic client registration, tokens (access + refresh), the in-flight PKCE
 * verifier and state nonce. Tokens reach the remote transport only — never the
 * model's context. The `state` round-trip encodes which
 * server the callback belongs to ("<server>.<nonce>") and the stored nonce must
 * match, so a forged callback can't graft tokens onto another entry.
 */
import { join } from "node:path";
import { chmodSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { auth, type OAuthClientProvider } from "@modelcontextprotocol/sdk/client/auth.js";
import type {
  OAuthClientInformationMixed,
  OAuthClientMetadata,
  OAuthTokens,
} from "@modelcontextprotocol/sdk/shared/auth.js";
import { mcpDir } from "./config.ts";

type FetchLike = (input: string | URL, init?: RequestInit) => Promise<Response>;

interface Stored {
  client?: OAuthClientInformationMixed;
  tokens?: OAuthTokens;
  codeVerifier?: string;
  /** Nonce of the in-flight authorization, cleared when tokens land. */
  state?: string;
}

function tokensDir(): string {
  return join(mcpDir(), "tokens");
}

function fileFor(server: string): string {
  return join(tokensDir(), `${server}.json`);
}

function load(server: string): Stored {
  try {
    return JSON.parse(readFileSync(fileFor(server), "utf-8"));
  } catch {
    return {};
  }
}

function save(server: string, stored: Stored): void {
  mkdirSync(tokensDir(), { recursive: true, mode: 0o700 });
  writeFileSync(fileFor(server), JSON.stringify(stored, null, 2) + "\n");
  chmodSync(fileFor(server), 0o600);
}

function patch(server: string, delta: Partial<Stored>): void {
  save(server, { ...load(server), ...delta });
}

/** The redirect target — bough's own HTTP surface (server/app.ts hosts it). */
export function callbackUrl(): string {
  return `http://127.0.0.1:${Deno.env.get("BOUGH_PORT") ?? 4321}/mcp/oauth/callback`;
}

export class BoughOAuthProvider implements OAuthClientProvider {
  /** Set when auth() wants the user agent sent somewhere — we capture, not redirect. */
  authorizationUrl?: URL;

  constructor(readonly server: string) {}

  get redirectUrl(): string {
    return callbackUrl();
  }

  get clientMetadata(): OAuthClientMetadata {
    return {
      client_name: "bough",
      redirect_uris: [callbackUrl()],
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
      token_endpoint_auth_method: "none", // public client — PKCE carries the proof
    };
  }

  state(): string {
    const nonce = crypto.randomUUID();
    patch(this.server, { state: nonce });
    return `${this.server}.${nonce}`;
  }

  clientInformation(): OAuthClientInformationMixed | undefined {
    return load(this.server).client;
  }

  saveClientInformation(client: OAuthClientInformationMixed): void {
    patch(this.server, { client });
  }

  tokens(): OAuthTokens | undefined {
    return load(this.server).tokens;
  }

  saveTokens(tokens: OAuthTokens): void {
    // Tokens landing means the in-flight authorization is finished — drop its nonce.
    save(this.server, { ...load(this.server), tokens, state: undefined });
  }

  redirectToAuthorization(url: URL): void {
    this.authorizationUrl = url;
  }

  saveCodeVerifier(codeVerifier: string): void {
    patch(this.server, { codeVerifier });
  }

  codeVerifier(): string {
    const v = load(this.server).codeVerifier;
    if (!v) throw new Error(`no PKCE verifier stored for ${this.server} — restart the auth flow`);
    return v;
  }
}

export interface AuthStart {
  status: "authorized" | "redirect";
  /** Present for "redirect": the URL the human must open to approve access. */
  authorizationUrl?: string;
}

/**
 * Start (or silently finish) the OAuth flow for a remote server. "authorized"
 * when stored tokens are valid or refreshable; "redirect" hands back the
 * authorization URL for the human. Discovery, registration, and PKCE prep all
 * happen inside the SDK's auth().
 */
export async function beginAuth(
  server: string,
  serverUrl: string,
  fetchFn?: FetchLike,
): Promise<AuthStart> {
  const provider = new BoughOAuthProvider(server);
  const result = await auth(provider, { serverUrl, fetchFn });
  if (result === "AUTHORIZED") return { status: "authorized" };
  if (!provider.authorizationUrl) {
    throw new Error("authorization server produced no authorization URL");
  }
  return { status: "redirect", authorizationUrl: String(provider.authorizationUrl) };
}

/**
 * Finish the flow from the browser callback: validate the state round-trip
 * ("<server>.<nonce>" minted by BoughOAuthProvider.state), exchange the code,
 * persist tokens. Returns the server name the tokens belong to.
 */
export async function completeAuth(
  state: string,
  code: string,
  serverUrlFor: (server: string) => string | undefined,
  fetchFn?: FetchLike,
): Promise<string> {
  const dot = state.lastIndexOf(".");
  if (dot <= 0) throw new Error("malformed state");
  const server = state.slice(0, dot);
  const nonce = state.slice(dot + 1);
  if (!nonce || load(server).state !== nonce) {
    throw new Error("state mismatch — restart the auth flow");
  }
  const serverUrl = serverUrlFor(server);
  if (!serverUrl) throw new Error(`"${server}" is not a registered remote mcp server`);
  const provider = new BoughOAuthProvider(server);
  const result = await auth(provider, { serverUrl, authorizationCode: code, fetchFn });
  if (result !== "AUTHORIZED") throw new Error("token exchange did not complete");
  return server;
}

export function hasTokens(server: string): boolean {
  return load(server).tokens !== undefined;
}

/** Forget a server's registration + tokens ("logout"). */
export function clearAuth(server: string): void {
  try {
    rmSync(fileFor(server));
  } catch {
    // nothing stored — already clear
  }
}
