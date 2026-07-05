import { assertEquals } from "jsr:@std/assert@1";
import { compileRule, decide, policy } from "../net/policy.ts";
import { kindFromAnnotations, mcpRequest, mcpVerb } from "./gate.ts";

function decideCall(
  server: string,
  tool: string,
  args: unknown,
  annotations: Record<string, unknown> | undefined,
  pol: ReturnType<typeof policy>,
) {
  const { req, classifier } = mcpRequest(server, tool, args, kindFromAnnotations(annotations));
  return decide(req, pol, [classifier]);
}

Deno.test("annotations seed kind: readOnly→read, annotated→write, absent→unknown", () => {
  assertEquals(kindFromAnnotations({ readOnlyHint: true }), "read");
  assertEquals(kindFromAnnotations({ readOnlyHint: false, destructiveHint: true }), "write");
  assertEquals(kindFromAnnotations({ destructiveHint: false }), "write"); // not read-only
  assertEquals(kindFromAnnotations(undefined), "unknown");
});

Deno.test("mcp calls ride the mode baseline: reads pass, writes hold, unknown fails closed", () => {
  const review = policy({ mode: "review" });
  const read = decideCall("cdp", "take_snapshot", {}, { readOnlyHint: true }, review);
  assertEquals([read.verdict, read.action.verb], ["allow", "mcp:cdp:take_snapshot"]);

  const write = decideCall("cdp", "click", { uid: "3" }, { readOnlyHint: false }, review);
  assertEquals([write.verdict, write.action.kind], ["hold", "write"]);

  const unknown = decideCall("cdp", "mystery", {}, undefined, review);
  assertEquals([unknown.verdict, unknown.action.kind], ["hold", "unknown"]);
  assertEquals(
    decideCall("cdp", "mystery", {}, undefined, policy({ mode: "read_only" })).verdict,
    "deny",
  );
});

Deno.test("the *.mcp claim skips the host allowlist, like an active plugin's", () => {
  // Only example.com is allowlisted — an HTTP request to cdp.mcp would hostMiss,
  // but the injected classifier claims the pseudo-host, so the call reaches the
  // action layer and classifies properly.
  const pol = policy({ mode: "review", allowHosts: new Set(["example.com"]) });
  const d = decideCall("cdp", "take_snapshot", {}, { readOnlyHint: true }, pol);
  assertEquals([d.verdict, d.action.service], ["allow", "mcp:cdp"]);
});

Deno.test("per-branch verb overrides and condition rules target mcp verbs", () => {
  assertEquals(mcpVerb("cdp", "click"), "mcp:cdp:click");
  const denied = decideCall(
    "cdp",
    "take_snapshot",
    {},
    { readOnlyHint: true },
    policy({ mode: "review", denyVerbs: new Set(["mcp:cdp:take_snapshot"]) }),
  );
  assertEquals(denied.verdict, "deny");

  // Condition rules see the args via the http facet's body_json.
  const pol = policy({
    mode: "review",
    rules: [compileRule({
      name: "no-file-uploads",
      hosts: ["cdp.mcp"],
      condition: 'has(http.body_json.path) && http.body_json.path.contains("/etc")',
      verdict: "deny",
    })],
  });
  const hit = decideCall("cdp", "upload_file", { path: "/etc/passwd" }, {
    readOnlyHint: true,
  }, pol);
  assertEquals([hit.verdict, hit.rule], ["deny", "no-file-uploads"]);
  const miss = decideCall("cdp", "upload_file", { path: "/tmp/x" }, { readOnlyHint: true }, pol);
  assertEquals(miss.verdict, "allow");
});
