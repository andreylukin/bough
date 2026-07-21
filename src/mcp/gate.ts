/**
 * The Claw Patrol border for MCP: every tool call is gated BEFORE it reaches the
 * server, through the same decide()/hold machinery as HTTP egress (net/gate.ts).
 *
 * Why here and not the proxy: seatbelt + proxy confinement borders the server's own
 * process, but MCP effects happen elsewhere — a stdio server drives an unsandboxed
 * app over loopback (chrome-devtools → Chrome via CDP), and a remote server acts
 * server-side behind one opaque JSON-RPC POST. The tool call is the only point where
 * the action is legible, so the call IS the request:
 *
 *   host = "<server>.mcp"            (rules can scope to it; feed rows show it)
 *   verb = "mcp:<server>:<tool>"     (holdVerbs/denyVerbs can target it per-branch)
 *   kind = from the tool's MCP annotations — readOnlyHint → read, annotated but not
 *          read-only → write, NO annotations → unknown, which FAILS CLOSED under
 *          read_only/review exactly like an unmatched plugin op. Annotations are
 *          server-supplied hints: they seed classification, they never grant.
 *   body = the call arguments, so condition rules can match on http.body_json.
 *
 * The injected classifier claims "*.mcp", which (like an active plugin's claim)
 * skips the allowHosts gate — activating the server was the trust decision. With no
 * gateway running there is no gate to consult and calls pass, the same posture as
 * bash egress running unrouted when Claw Patrol is off.
 */
import { activeGateway } from "../net/gateway.ts";
import {
  type Action,
  type Classifier,
  type Kind,
  READ,
  type Request,
  UNKNOWN,
  WRITE,
} from "../net/policy.ts";

/** Kind seeded from MCP tool annotations — a hint for classification, not a grant. */
export function kindFromAnnotations(annotations: Record<string, unknown> | undefined): Kind {
  if (!annotations) return UNKNOWN;
  return annotations.readOnlyHint === true ? READ : WRITE;
}

export function mcpVerb(server: string, tool: string): string {
  return `mcp:${server}:${tool}`;
}

/** The pseudo-request + classifier pair one call presents to decide(). */
export function mcpRequest(
  server: string,
  tool: string,
  args: unknown,
  kind: Kind,
): { req: Request; classifier: Classifier } {
  const host = `${server}.mcp`;
  const action: Action = {
    service: `mcp:${server}`,
    verb: mcpVerb(server, tool),
    kind,
    facet: { name: "mcp", fields: { server, tool } },
  };
  return {
    req: { host, method: "CALL", path: `/${tool}`, body: JSON.stringify(args ?? {}) },
    classifier: { name: "mcp", hosts: [host], classify: () => action },
  };
}

/**
 * Gate one MCP tool call for a session. Resolves when the call may proceed (a hold
 * blocks here until the human approves, exactly like a held HTTP request); throws
 * with the policy reason when denied.
 */
export async function gateMcpCall(
  sessionId: string | undefined,
  server: string,
  tool: string,
  args: unknown,
  annotations: Record<string, unknown> | undefined,
): Promise<void> {
  const gate = activeGateway()?.gate;
  if (!gate) return; // Claw Patrol off — same unrouted posture as bash egress
  const { req, classifier } = mcpRequest(server, tool, args, kindFromAnnotations(annotations));
  const decision = await gate.gate(req, {
    sessionId,
    requestedBy: "mcp",
    classifiers: [classifier],
  });
  if (decision.verdict !== "allow") {
    throw new Error(`mcp call ${mcpVerb(server, tool)} blocked by Claw Patrol: ${decision.reason}`);
  }
}
