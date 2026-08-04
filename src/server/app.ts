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
 * **The route table is read while this module evaluates**, which is why the handler
 * modules below must never import back from here. `route(..., sessions.listSessions)`
 * dereferences a binding at module scope, so a graph entered through a handler module
 * would evaluate this file mid-way through that module's body and read a `const`
 * that has not been assigned — a `ReferenceError` at import, before any user code.
 * The primitives every handler needs therefore live in `server/http.ts`, which
 * imports nothing from `server/`; handlers reach DOWN to it instead of back across to
 * here, and the graph is a DAG whichever module the process enters through. They are
 * re-exported below so the old spelling still resolves, and `server/http.test.ts`
 * fails if a module reintroduces the edge.
 *
 * **No CORS headers, ever.** The only client is the native TUI, which is not a
 * browser and needs none. Their absence is what stops a webpage the user happens
 * to visit from reaching this loopback API and driving the agent: without an
 * allow-origin header the browser blocks the cross-origin read. The server binds
 * loopback and has no auth layer (spec §17), so this is the whole of its access
 * control and it is worth not undoing by reflex.
 */
import { HttpError } from "../errors.ts";
import type { AppCtx } from "../types.ts";
import { errorResponse, type Handler, type Route, route } from "./http.ts";
// Re-exported so `import { json, parseBody, route, type Handler } from "./app.ts"`
// keeps resolving for anything not yet moved. New code imports `./http.ts` directly
// — importing them from HERE re-forms the initialization cycle documented above,
// and `server/http.test.ts` is what catches it.
export { errorResponse, type Handler, json, parseBody, type Route, route } from "./http.ts";
// ── handler imports; append below, one line per task ──
import { events } from "./events.ts"; // T1.3
import * as fsapi from "./fs.ts";
import * as models from "./models.ts";
import * as sessions from "./sessions.ts"; // T1.2
import * as workflows from "./workflows.ts"; // T5.5
import * as questions from "./questions.ts"; // T6.1
import * as schedules from "../schedules.ts"; // T6.3
import * as artifacts from "./artifacts.ts"; // T6.6
import * as comments from "./comments.ts"; // T6.7
import * as jobsApi from "./jobs.ts"; // T6.8
import * as relaunch from "../workflow/relaunch.ts"; // T5.7
import * as mcpOauth from "../mcp/oauth.ts"; // T7.2
import * as mcpApi from "../mcp/status.ts"; // T7.3
import * as compact from "../history/compact.ts"; // T8.3
import * as sections from "../history/sections.ts"; // T8.3
import * as fork from "../history/fork.ts"; // T8.2
import * as unsend from "../history/unsend.ts";
import * as extract from "../history/extract.ts"; // T8.4
import * as move from "../history/move.ts"; // T8.4
import * as handoff from "../history/handoff.ts"; // T8.4
import * as changes from "./changes.ts"; // T8.5
import * as search from "./search.ts"; // T8.6
import * as skillsApi from "./skills.ts"; // T10.2
import * as theme from "./theme.ts"; // T10.4
import * as ghost from "../worker/ghost.ts"; // T10.1
import * as turnsApi from "./turns.ts"; // final integration — the interrupt route
import * as attachments from "./attachments.ts"; // composer clipboard images

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
  route("PATCH", "/sessions/:id", sessions.patchSession),
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
  route("POST", "/sessions/:id/jobs", jobsApi.runShellH),
  route("POST", "/sessions/:id/jobs/:jobId/kill", jobsApi.killJobH),
  route("GET", "/sessions/:id/jobs/:jobId/output", jobsApi.jobOutputH),
  // T5.7 — relaunch from a journal, with prefix-bounded replay. `relaunch` is the
  // generalization of `rerun`: a NEW run seeded from a stopped run's journal, whose
  // unchanged leading calls replay and whose first changed call — and everything after
  // it — runs live. `/workflows/:id/replay` is the required counterpart: replay is
  // always reported (spec §8), and a run that replayed nothing is otherwise
  // indistinguishable from one that replayed everything.
  route("POST", "/workflows/:id/relaunch", relaunch.relaunchWorkflowH),
  route("GET", "/workflows/:id/replay", relaunch.workflowReplayH),
  // T5.8 — saving a run as a named workflow, and the cost surface's one setting.
  //
  // `/saved-workflows` is a top-level collection rather than `/workflows/saved`
  // because this table is matched in order and appends land at the END: a two-segment
  // `/workflows/saved` would be swallowed by `/workflows/:id` above and answer 404 for
  // a run id of "saved". The same reasoning puts the guideline at `/workflow-settings`.
  route("POST", "/workflows/:id/save", workflows.saveWorkflowH),
  route("GET", "/saved-workflows", workflows.listSavedWorkflowsH),
  route("GET", "/saved-workflows/:name", workflows.getSavedWorkflowH),
  route("PUT", "/saved-workflows/:name", workflows.putSavedWorkflowH),
  route("POST", "/saved-workflows/:name/runs", workflows.runSavedWorkflowH),
  // What the picker may CHOOSE from, as opposed to what is currently chosen. Answered
  // here and not compiled into the TUI because the key that decides the answer is the
  // server's (`server/models.ts`).
  route("GET", "/models", models.getModelsH),
  route("GET", "/model-settings", sessions.getModelSettingsH),
  // The write half. Without it a chosen model lasted one conversation: `ctx.model`
  // is `BOUGH_MODEL` frozen at start-up, so the next session reverted to the
  // built-in default and the picker looked broken because it was.
  route("PUT", "/model-settings", sessions.putModelSettingsH),
  // Candidates for the composer's `@` completion (`server/fs.ts`).
  route("GET", "/sessions/:id/files", fsapi.listFilesH),
  route("GET", "/files", fsapi.listFilesForWorkspaceH),
  // The same completion, for a path that leaves the workspace (`@~/…`, `@/…`).
  route("GET", "/fs/entries", fsapi.listDirEntriesH),
  // The branch the meter names beside the workspace.
  route("GET", "/fs/branch", fsapi.branchH),
  route("GET", "/workflow-settings", workflows.getWorkflowSettingsH),
  route("PUT", "/workflow-settings", workflows.putWorkflowSettingsH),
  // T7.2 — OAuth for remote MCP servers. The callback is the load-bearing one: the
  // authorization server sends the user's BROWSER back here, so this path is baked
  // into the redirect URI bough registers and must exist on bough's own port (spec
  // §10). The three `/mcp/servers/:name/auth` verbs are what the mcp panel's `a`/`F`
  // drives — start the flow, read its state, forget the tokens. No route here ever
  // returns a token; they return a URL for the human and a status for the panel.
  route("GET", mcpOauth.CALLBACK_PATH, mcpOauth.oauthCallbackH),
  route("GET", "/mcp/servers/:name/auth", mcpOauth.authStatusH),
  route("POST", "/mcp/servers/:name/auth", mcpOauth.beginAuthH),
  route("DELETE", "/mcp/servers/:name/auth", mcpOauth.clearAuthH),
  // T7.3 — the MCP registry, the per-session grants over it, and live connections.
  // Registering is not granting (`mcp/config.ts`): `PUT` defines a server, `enable`
  // is what lets a session's programs call it, and `connect` only proves the command
  // works. Every one of these answers with the SAME `{registry, auth, active,
  // connections}` document `bough mcp` renders, so the human's panel and the model's
  // fresh `bough mcp call` can never be looking at different MCP states (plan §6.13).
  // The `/auth` verbs above are T7.2's and are deliberately not restated here.
  route("GET", "/mcp/servers", mcpApi.getMcpServersH),
  route("PUT", "/mcp/servers", mcpApi.putMcpServersH),
  route("PUT", "/mcp/servers/:name", mcpApi.putMcpServerH),
  route("DELETE", "/mcp/servers/:name", mcpApi.deleteMcpServerH),
  route("POST", "/mcp/servers/:name/connect", mcpApi.connectMcpServerH),
  // Calling a tool. More specific than `/connect` in shape but not in prefix, so
  // ordering against it does not matter; placed beside it because it is the same
  // family. The grant is enforced in the handler, not here.
  route("POST", "/mcp/servers/:name/tools/:tool", mcpApi.callMcpToolH),
  route("POST", "/mcp/servers/:name/restart", mcpApi.restartMcpServerH),
  route("POST", "/mcp/servers/:name/enable", mcpApi.setMcpActivationH(true)),
  route("POST", "/mcp/servers/:name/disable", mcpApi.setMcpActivationH(false)),
  // T8.2 — fork at message, and edit-and-resend. A history operation is a POST that
  // CREATES a session (201) and never mutates the one in the URL: the source is
  // byte-identical afterwards, so this is safe to offer on any turn, however far back.
  route("POST", "/sessions/:id/fork", fork.forkSessionH),
  // T8.3 — compaction and topic sections. Compaction is the same shape as fork: a POST
  // that CREATES a compaction branch (201) and leaves the session in the URL
  // byte-identical, because every history operation branches and none rewrites (spec
  // §14). Sections is the odd one — it is STATELESS: the client sends turn gists, gets
  // back labeled ranges, and nothing is read from or written to the session. It is
  // nested under `/sessions/:id` anyway so a mistyped id 404s instead of buying an LLM
  // call about a thread nobody is looking at.
  route("POST", "/sessions/:id/compact", compact.compactH),
  route("POST", "/sessions/:id/sections", sections.sectionsH),
  // T8.4 — extract, move-into, handoff: the three history ops whose selection may reach
  // into ANCESTOR history, which fork and compaction cannot (spec §14). All three leave
  // the session in the URL byte-identical. `extract` and `handoff` CREATE a fresh root
  // (201); `move-into` creates nothing (200) — it appends copies onto the session named
  // by `:id`, and the session copied FROM travels in the body as `sourceId`, because it
  // is the argument rather than the thing being acted on.
  route("POST", "/sessions/:id/extract", extract.extractH),
  route("POST", "/sessions/:id/move-into", move.moveIntoH),
  route("POST", "/sessions/:id/handoff", handoff.handoffH),
  // T8.5 — the Changes rail. Two routes, because there are only two operations: read
  // what this session changed, and revert some of it. There is no apply — the agent
  // edits the user's checkout in place, so the work is already delivered and
  // committing is the reviewer's own call (spec §13, §17).
  //
  // GET is always 200: a workspace that is not a repository has no change set, and
  // saying so plainly is an ANSWER, not an error. The only 400 is a revert asked of a
  // session that has nothing to revert against.
  route("GET", "/sessions/:id/changes", changes.getChangesH),
  route("POST", "/sessions/:id/changes/revert", changes.revertChangesH),
  // T8.6 — keyword search over transcripts (spec §17: FTS, no embeddings). Top-level
  // rather than nested under a session because the question it answers is "did I solve
  // this before?", which spans the whole forest; `?sessionId=` narrows it back down.
  // `/search/reindex` is the repair path for the drift a swallowed index write leaves
  // behind — search is allowed to fail quietly, never invisibly (`server/search.ts`).
  route("GET", "/search", search.searchH),
  route("POST", "/search/reindex", search.reindexH),
  // T10.4 — theming. A theme is a NAMED PARTIAL palette over a fixed semantic token set
  // and is pure DATA: the TUI fetches this at boot and paints truecolor, so adopting one
  // is a repaint rather than a rebuild (spec §16). Top-level and singular because there
  // is exactly one theme per install — it is not per-session, and the picker previews it
  // client-side without touching these routes until the user commits.
  //
  // GET is always 200 even with nothing stored: "no theme" is the default palette, which
  // is an answer. DELETE is idempotent for the same reason.
  route("GET", "/theme", theme.getThemeH),
  route("PUT", "/theme", theme.putThemeH),
  route("DELETE", "/theme", theme.deleteThemeH),
  // T10.1 — composer ghost text, one of the three cheap-tier micro-tasks (spec §12).
  // POST rather than GET despite reading nothing: the half-typed prefix is user text
  // that has no business in a URL or an access log. ALWAYS 200 for a session that
  // exists, with `{ghost: null}` standing in for every failure there is — a cheap-model
  // outcome must never reach the composer as an error banner.
  route("POST", "/sessions/:id/ghost", ghost.ghostTextH),
  // T10.2 — skills. Top-level and session-less on purpose: a skill is a folder on
  // disk, not a row (`db/schema.sql`), and what is installed is a property of the
  // machine rather than of a conversation. Both routes are a fresh walk of the source
  // directories, so a skill written a second ago is listed a second later.
  //
  // GET is always 200 with a possibly-empty list; a malformed SKILL.md is a row with
  // an `error`, never an omission. `/skills/:name` adds the body with `${SKILL_DIR}`
  // resolved — the two questions a listing cannot answer (`server/skills.ts`).
  route("GET", "/skills", skillsApi.listSkillsH),
  route("GET", "/skills/:name", skillsApi.getSkillH),
  // FINAL INTEGRATION — the user interrupt spec §5 requires. `turn/runner.ts` has
  // exported `interruptTurn` since M2 and nothing reached it: both clients carried a
  // "KNOWN GAP" note in their headers instead of a stop button, which meant a running
  // turn could only be stopped by killing the server. Always 200 for a session that
  // exists — "nothing was running" is an answer, not a race the client must handle
  // (`server/turns.ts`).
  route("POST", "/sessions/:id/interrupt", turnsApi.interruptSession),
  // The live cost meter (spec §9). `GET /sessions/:id` already carries these two
  // totals, but it carries the whole assembled thread with them; this is the same
  // answer small enough to poll every few seconds while a turn is running, which is
  // what lets the running line say what the turn has spent SO FAR instead of only
  // after it settles (`server/sessions.ts`).
  route("GET", "/sessions/:id/usage", sessions.getSessionUsageH),
  // Native TUI clipboard images arrive as bytes once, then become durable paths.
  route("POST", "/attachments", attachments.uploadAttachment),
  // The take-back (`history/unsend.ts`). The one route that deletes messages rather
  // than branching, and the rules that make that safe live in that module, not here:
  // the session's own last USER message, plus whatever followed it, within the
  // gesture the TUI arms for `UNSEND_MS` after a send. Escape used to answer this by
  // forking, which left a sibling conversation behind for a message that existed for
  // three seconds.
  route("POST", "/sessions/:id/unsend", unsend.unsendMessageH),
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
 * Build the fetch handler bound to a ctx. `main.ts` passes it to `Bun.serve`;
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
