/**
 * The HTTP primitives every handler needs: the handler shape, the route
 * constructor, and the three response helpers.
 *
 * THE INVARIANT THIS HOLDS: **nothing a handler module imports may import a
 * handler module back.** This file exists for exactly one reason, and it is not
 * tidiness — it is module initialization order.
 *
 * `server/app.ts` builds its route table at module scope, and that table names a
 * function from every handler module in the tree. When those same handler modules
 * reached back into `app.ts` for `json`/`parseBody`/`Handler`, every one of them
 * closed a cycle. A cycle is not automatically fatal — ES modules tolerate one as
 * long as nothing is READ during evaluation — but this one is read during
 * evaluation: `route("GET", "/sessions", sessions.listSessions)` dereferences a
 * binding while the table is being built. So whether the server starts came down to
 * which module the graph happened to enter first:
 *
 *   - enter through `app.ts` (what `main.ts` does) → handlers evaluate first, their
 *     bindings initialize, the table reads them, everything works;
 *   - enter through any handler module → `app.ts` evaluates in the middle of that
 *     module's body, the table reads a `const` that has not been assigned yet, and
 *     the process dies with `ReferenceError: Cannot access 'listSessions' before
 *     initialization` before a line of user code runs.
 *
 * That was reproducible: a two-line script importing `server/sessions.ts` threw on
 * import. It had been flagged in two phases and worked only by accident, and the
 * accident was one new import away from breaking — a test, a script, or a module
 * that legitimately wanted `sessions.ts` was enough to take the server down.
 *
 * The fix is structural rather than a rule nobody can enforce by reading. These
 * helpers have no dependencies of their own beyond `errors.ts` and Zod, so a handler
 * module importing them reaches DOWN the graph instead of back across it. `app.ts`
 * now imports handlers and this file, handlers import this file, and the graph is a
 * DAG whichever module the process enters through. `server/http.test.ts` pins both
 * halves: the helpers behave, and no module under `src/` imports `app.ts` except
 * the entry points that are allowed to.
 *
 * `app.ts` re-exports everything here, so the older `import { json } from "./app.ts"`
 * spelling still resolves for a caller that has not been moved — but it re-forms the
 * cycle, which is why the guard test names the offender rather than trusting the
 * re-export to make it harmless.
 */
import type { z } from "zod";
import { BadRequestError } from "../errors.ts";
import type { AppCtx } from "../types.ts";

// ---- the handler shape ------------------------------------------------------

/**
 * Every endpoint is `(req, ctx, params)`. `params` holds the pattern's named
 * groups, already narrowed to the ones that actually matched — an optional group
 * that did not match is absent rather than present-and-undefined, so a handler can
 * write `params.path ?? ""` and mean it.
 */
export type Handler = (
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
) => Response | Promise<Response>;

export interface Route {
  /** Matched exactly against `req.method`. */
  method: string;
  pattern: URLPattern;
  handler: Handler;
}

/**
 * Build one route entry. Appending a one-line `route("GET", "/x", getX)` instead
 * of a five-line object literal is not cosmetics: it keeps each task's addition to
 * a single line, which is what makes concurrent appends to a shared array merge
 * without conflict.
 */
export function route(method: string, pathname: string, handler: Handler): Route {
  return { method, pattern: new URLPattern({ pathname }), handler };
}

// ---- response helpers -------------------------------------------------------

export function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

/**
 * The error envelope: `{error: message}`, which is the shape every client reads.
 *
 * Prefer throwing the domain error — `throw new NotFoundError("session not found")`
 * — over returning this. The throw is what lets a domain module state its HTTP
 * contract without importing anything from `server/`; this helper exists for the
 * dispatcher itself and for the rare handler that has a status but no domain error
 * to name.
 */
export function errorResponse(status: number, message: string): Response {
  return json({ error: message }, status);
}

/**
 * Parse and validate a JSON request body.
 *
 * A failed parse throws the 400 that the dispatcher's one catch renders, so no
 * handler branches on validation. `fallback` stands in for an absent or
 * unparseable body: the default `null` lets the schema decide (an all-optional
 * body would reject it, so a route with no required fields passes `{}`).
 */
export async function parseBody<S extends z.ZodTypeAny>(
  req: Request,
  schema: S,
  fallback: unknown = null,
): Promise<z.infer<S>> {
  const parsed = schema.safeParse(await req.json().catch(() => fallback));
  if (!parsed.success) throw new BadRequestError("invalid body: " + parsed.error.message);
  return parsed.data;
}
