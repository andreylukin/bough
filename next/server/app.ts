/**
 * The HTTP surface: one route table, one dispatcher, one try/catch.
 *
 * The invariant this holds is that **HTTP lives here and nowhere else.** A domain
 * module signals failure by throwing an `HttpError` subclass carrying its status
 * (`errors.ts`); the single catch below is what turns that into a response. No
 * handler contains a per-error catch block, and no module outside `server/`
 * constructs a `Response` — which is what keeps `history/`, `hostfn/` and the rest
 * unit-testable with no server in sight.
 *
 * Second invariant, and the reason `createHandler` takes a ctx rather than
 * reaching for module state: **every dependency arrives as a parameter.** The
 * database, the bus, the LLM client and the clock all travel in `AppCtx`, so a
 * test builds a fabricated ctx over an in-memory database, calls the returned
 * function with a `Request`, and asserts on the `Response` — with no socket bound,
 * no port claimed, and nothing on the network (plan §7, "Server: `createHandler(ctx)`
 * with a fabricated ctx and an in-memory db. No socket.").
 *
 * **The route table is APPEND-ONLY** (plan §4). This file is shared by every task
 * that adds an endpoint; the merge discipline that keeps parallel agents from
 * colliding is that a task appends its entries at the very end of `routes` and
 * writes its handler in its own file. Nothing is ever reordered and no task edits
 * another task's entry. See the marker at the bottom of the array.
 *
 * Matching is on the pathname only — `URLPattern` rejects an init object and a
 * base URL together — and in table order, first match wins. That is also why order
 * is never rearranged: a reorder can silently steal another task's route.
 *
 * **No CORS headers, ever.** The only client is the native TUI, which is not a
 * browser and needs none. Their absence is what stops a webpage the user happens
 * to visit from reaching this loopback API and driving the agent: without an
 * allow-origin header the browser blocks the cross-origin read. The server binds
 * loopback and has no auth layer (spec §17), so this is the whole of its access
 * control and it is worth not undoing by reflex.
 */
import type { z } from "zod";
import { BadRequestError, HttpError } from "../errors.ts";
import type { AppCtx } from "../types.ts";
// ── handler imports; append below, one line per task ──
import { events } from "./events.ts"; // T1.3
import * as sessions from "./sessions.ts"; // T1.2
import * as workflows from "./workflows.ts"; // T5.5
import * as questions from "./questions.ts"; // T6.1
import * as schedules from "../schedules.ts"; // T6.3
import * as artifacts from "./artifacts.ts"; // T6.6
import * as comments from "./comments.ts"; // T6.7
import * as jobsApi from "./jobs.ts"; // T6.8

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
//
// Exported for the handler modules that later tasks own. Importing these from a
// module that this file also imports forms a cycle, which is safe here and only
// here: both are called at REQUEST time, never while a module is evaluating, so
// the binding is always initialized by the time it is read.

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

// ---- the route table --------------------------------------------------------

/**
 * APPEND-ONLY. Add your task's entries at the end, below the marker. Never
 * reorder, never edit another task's entry, never insert in the middle — a
 * reorder changes which route wins for an overlapping pathname, and an insert
 * conflicts with every other agent appending at the same time.
 *
 * The table starts empty on purpose: `GET /` and the 404 are fallbacks in
 * `createHandler`, not entries, so every line here belongs to exactly one task.
 */
export const routes: Route[] = [
  // ── append new routes below this line, never above it ──
  route("GET", "/events", events), // T1.3
  // T1.2 — sessions and messages
  route("GET", "/sessions", sessions.listSessions),
  route("POST", "/sessions", sessions.createSession),
  route("GET", "/sessions/:id", sessions.getSession),
  route("POST", "/sessions/:id/messages", sessions.postMessage),
  route("PUT", "/sessions/:id/draft", sessions.putDraft),
  // T5.5 — workflows. `/workflows/:id/agents/:agentId/:action` is listed after the
  // lifecycle verbs; it cannot shadow them (a different segment count), and the
  // order is the append order either way.
  route("GET", "/workflows", workflows.listWorkflows),
  route("POST", "/workflows", workflows.createWorkflow),
  route("GET", "/workflows/:id", workflows.getWorkflow),
  route("POST", "/workflows/:id/stop", workflows.stopWorkflowH),
  route("POST", "/workflows/:id/pause", workflows.pauseWorkflowH),
  route("POST", "/workflows/:id/resume", workflows.resumeWorkflowH),
  route("POST", "/workflows/:id/rerun", workflows.rerunWorkflowH),
  route("POST", "/workflows/:id/agents/:agentId/:action", workflows.controlWorkflowAgentH),
  // T6.3 — schedules. The same validated CRUD the `schedule.*` host fn uses, so a
  // spec that parses over HTTP parses from a program and vice versa.
  route("GET", "/schedules", schedules.listSchedulesH),
  route("POST", "/schedules", schedules.createScheduleH),
  route("PATCH", "/schedules/:id", schedules.patchScheduleH),
  route("DELETE", "/schedules/:id", schedules.deleteScheduleH),
  // T6.1 — ask() holds. Memory-only: GET rebuilds a freshly-attached client's hold
  // card, POST settles one and the parked program resumes.
  route("GET", "/questions", questions.listQuestions),
  route("POST", "/sessions/:id/questions/:qid", questions.answerQuestion),
  // T6.6 — artifacts. The listing is filesystem-backed and survives a database reset,
  // so it deliberately does not require a session row. `/artifacts/:id/:path*` is the
  // hosted file itself, same origin as the API so a printed link just opens.
  route("GET", "/sessions/:id/artifacts", artifacts.listArtifactsH),
  route("GET", "/artifacts/:id/:path*", artifacts.getArtifactH),
  // T6.7 — artifact comments. These are what the layer injected into every served HTML
  // artifact talks to. `/comments/send` is listed before `/comments/:cid` for reading
  // order; it cannot be shadowed by it either way (different method).
  route("GET", "/sessions/:id/comments", comments.listCommentsH),
  route("POST", "/sessions/:id/comments", comments.postCommentH),
  route("POST", "/sessions/:id/comments/send", comments.sendCommentsH),
  route("DELETE", "/sessions/:id/comments/:cid", comments.deleteCommentH),
  // T6.8 — jobs. Scoped to a session AND its subagents, so a spawner's jobs tab shows
  // the work running on its behalf and the human can kill it without spending a turn.
  route("GET", "/sessions/:id/jobs", jobsApi.listJobsH),
  route("POST", "/sessions/:id/jobs/:jobId/kill", jobsApi.killJobH),
  route("GET", "/sessions/:id/jobs/:jobId/output", jobsApi.jobOutputH),
];

// ---- dispatch ---------------------------------------------------------------

/** The pointer served at `GET /`. There is no web UI; this origin is the API. */
const ROOT_POINTER = "bough server — drive it with the `bough` TUI.\n" +
  "There is no web UI: this origin is the JSON API, the /events SSE stream, and " +
  "artifact hosting.\n";

export interface CreateHandlerOptions {
  /**
   * Override the route table. Production takes the default; a router test passes
   * a fabricated table so it exercises dispatch, params and error mapping without
   * depending on which endpoints happen to exist yet.
   */
  routes?: readonly Route[];
  /**
   * Where a non-`HttpError` escaping a handler is reported. Such an error is a
   * bug, not a domain outcome, so it is logged rather than swallowed; a test
   * passes a collector so an intentional throw does not print a stack and so the
   * isolation can be asserted instead of inferred.
   */
  onUnexpectedError?: (error: unknown, req: Request) => void;
}

/**
 * Build the fetch handler bound to a ctx. `main.ts` passes it to `Deno.serve`;
 * tests call the returned function directly with a `Request` and never bind a
 * socket.
 */
export function createHandler(
  ctx: AppCtx,
  opts: CreateHandlerOptions = {},
): (req: Request) => Promise<Response> {
  const table = opts.routes ?? routes;
  const onUnexpectedError = opts.onUnexpectedError ??
    ((err: unknown, req: Request) =>
      console.error(`unhandled error in ${req.method} ${new URL(req.url).pathname}:`, err));

  return async (req: Request): Promise<Response> => {
    const { pathname } = new URL(req.url);

    for (const entry of table) {
      if (entry.method !== req.method) continue;
      const match = entry.pattern.exec({ pathname });
      if (!match) continue;
      try {
        return await entry.handler(req, ctx, groupsOf(match));
      } catch (e) {
        // THE one catch. Domain errors carry their own status (errors.ts), which
        // is what lets every module below `server/` throw instead of returning a
        // Response. Anything else is a defect: report it and answer 500 rather
        // than letting the connection die with no body.
        if (e instanceof HttpError) return errorResponse(e.status, e.message);
        onUnexpectedError(e, req);
        return errorResponse(500, e instanceof Error ? e.message : String(e));
      }
    }

    if (req.method === "GET" && pathname === "/") {
      return new Response(ROOT_POINTER, {
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }

    // The path exists but not for this method. Saying so beats a 404 that reads
    // as "endpoint missing" and sends the caller looking for the wrong bug.
    const allowed = [
      ...new Set(table.filter((r) => r.pattern.exec({ pathname })).map((r) => r.method)),
    ];
    if (allowed.length > 0) {
      return new Response(
        JSON.stringify({
          error: `${req.method} not allowed on ${pathname} — try ${allowed.join(", ")}`,
        }),
        {
          status: 405,
          headers: {
            "content-type": "application/json; charset=utf-8",
            allow: allowed.join(", "),
          },
        },
      );
    }

    return errorResponse(404, `no route for ${req.method} ${pathname}`);
  };
}

/**
 * The matched named groups, minus the ones that did not participate. `URLPattern`
 * reports an unmatched optional group as `undefined`; carrying that through would
 * make `Record<string, string>` a lie that every handler has to remember.
 */
function groupsOf(match: URLPatternResult): Record<string, string> {
  const params: Record<string, string> = {};
  for (const [key, value] of Object.entries(match.pathname.groups)) {
    if (value !== undefined) params[key] = value;
  }
  return params;
}
