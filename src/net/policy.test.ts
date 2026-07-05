import { assert, assertEquals, assertThrows } from "jsr:@std/assert@1";
import { classifyK8s, compileRule, decide, policy, type Request } from "./policy.ts";
import { NetConfig, RuleConfig, toPolicy } from "./config.ts";

const get = (host: string, path: string): Request => ({ host, method: "GET", path });
const post = (host: string, path: string, body?: string): Request => ({
  host,
  method: "POST",
  path,
  body,
});

Deno.test("rules: first match wins and outranks mode", () => {
  const pol = policy({
    mode: "all",
    rules: [
      compileRule({
        name: "block-refunds",
        condition: "http.method == 'POST' && http.path.startsWith('/v1/refunds')",
        verdict: "deny",
      }),
      compileRule({ name: "allow-the-rest", condition: "true", verdict: "allow" }),
    ],
  });
  assertEquals(decide(post("api.stripe.example", "/v1/refunds/123"), pol).verdict, "deny");
  assertEquals(decide(post("api.stripe.example", "/v1/refunds/123"), pol).rule, "block-refunds");
  assertEquals(decide(get("api.stripe.example", "/v1/charges"), pol).rule, "allow-the-rest");
});

Deno.test("rules: hosts scope — a rule skips other hosts", () => {
  const pol = policy({
    mode: "all",
    rules: [
      compileRule({
        name: "gh-only",
        hosts: ["api.github.com"],
        condition: "http.method == 'DELETE'",
        verdict: "deny",
      }),
    ],
  });
  const del = (host: string): Request => ({ host, method: "DELETE", path: "/x" });
  assertEquals(decide(del("api.github.com"), pol).verdict, "deny");
  assertEquals(decide(del("other.example"), pol).verdict, "allow"); // mode=all
});

Deno.test("rules: unevaluable condition fails closed (deny)", () => {
  const pol = policy({
    mode: "all",
    rules: [
      compileRule({
        name: "needs-json",
        condition: "http.body_json.archived == true",
        verdict: "allow",
      }),
    ],
  });
  const d = decide(post("api.example.com", "/things", "not json"), pol);
  assertEquals(d.verdict, "deny");
  assert(d.reason.includes("unevaluable"));
});

Deno.test("rules: approve chain surfaces as hold + approve list", () => {
  const pol = policy({
    mode: "all",
    rules: [
      compileRule({
        name: "gated-delete",
        condition: "http.method == 'DELETE'",
        approve: ["plugin:age-check", "human"],
      }),
    ],
  });
  const d = decide({ host: "api.example.com", method: "DELETE", path: "/obj/1" }, pol);
  assertEquals(d.verdict, "hold");
  assertEquals(d.approve, ["plugin:age-check", "human"]);
});

Deno.test("rules: graphql facet + action env are visible to conditions", () => {
  const pol = policy({
    mode: "all",
    rules: [
      compileRule({
        name: "hold-mutations",
        condition: "has(graphql.operation) && graphql.operation == 'mutation'",
        verdict: "hold",
      }),
    ],
  });
  const mutation = post("api.github.com", "/graphql", '{"query":"mutation { x }"}');
  const query = post("api.github.com", "/graphql", '{"query":"query { x }"}');
  assertEquals(decide(mutation, pol).verdict, "hold");
  assertEquals(decide(query, pol).verdict, "allow");
  // a non-graphql request has no graphql facet — has() guards, rule skips
  assertEquals(decide(get("api.github.com", "/user"), pol).verdict, "allow");
});

Deno.test("k8s facet: verb/resource/namespace/name parsed from the path", () => {
  const req: Request = {
    host: "k8s.example",
    method: "POST",
    path: "/api/v1/namespaces/prod/pods/web-1/exec?container=app",
  };
  const action = classifyK8s(req);
  assertEquals(action.facet?.name, "k8s");
  assertEquals(action.facet?.fields, {
    verb: "create",
    resource: "pods/exec",
    namespace: "prod",
    name: "web-1",
  });
  // list form: no namespace, no name
  assertEquals(classifyK8s(get("k8s.example", "/api/v1/pods")).facet?.fields, {
    verb: "list",
    resource: "pods",
    namespace: "",
    name: "",
  });
  // group APIs
  assertEquals(
    classifyK8s(get("k8s.example", "/apis/apps/v1/namespaces/x/deployments/web")).facet?.fields,
    { verb: "get", resource: "deployments", namespace: "x", name: "web" },
  );
});

Deno.test("rules: k8s exec deniable via facet condition", () => {
  const pol = policy({
    k8sHosts: new Set(["k8s.example"]),
    mode: "review",
    rules: [
      compileRule({
        name: "no-exec",
        condition: "has(k8s.resource) && k8s.resource in ['pods/exec', 'pods/attach']",
        verdict: "deny",
      }),
    ],
  });
  const exec: Request = {
    host: "k8s.example",
    method: "POST",
    path: "/api/v1/namespaces/prod/pods/web-1/exec",
  };
  assertEquals(decide(exec, pol).verdict, "deny");
  // plain reads still pass
  assertEquals(decide(get("k8s.example", "/api/v1/pods"), pol).verdict, "allow");
});

Deno.test("NetConfig: rules round-trip and bad conditions are rejected at parse", () => {
  const cfg = NetConfig.parse({
    rules: [{ name: "r1", condition: "http.method == 'DELETE'", verdict: "deny" }],
  });
  const pol = toPolicy(cfg);
  assertEquals(decide({ host: "x.example", method: "DELETE", path: "/" }, pol).verdict, "deny");

  assertThrows(() =>
    NetConfig.parse({
      rules: [{ name: "bad", condition: "http.method ==", verdict: "deny" }],
    })
  );
  // exactly one of verdict/approve
  assertThrows(() => RuleConfig.parse({ name: "r", condition: "true" }));
  assertThrows(() =>
    RuleConfig.parse({ name: "r", condition: "true", verdict: "deny", approve: ["human"] })
  );
  // approver names are validated
  assertThrows(() => RuleConfig.parse({ name: "r", condition: "true", approve: ["robot"] }));
});

Deno.test("yolo: everything allowed — denyHosts, writes, rules all shadow-logged", () => {
  const pol = policy({
    mode: "yolo",
    denyHosts: new Set(["evil.example"]),
    rules: [
      compileRule({ name: "no-del", condition: "http.method == 'DELETE'", verdict: "deny" }),
    ],
  });
  // denied host: allowed, reason names the shadow verdict
  const blocked = decide(get("evil.example", "/x"), pol);
  assertEquals(blocked.verdict, "allow");
  assert(blocked.reason.includes("would have denied"));
  // rule hit: allowed, rule name still attributed
  const del = decide({ host: "api.example", method: "DELETE", path: "/y" }, pol);
  assertEquals(del.verdict, "allow");
  assert(del.reason.includes("would have denied"));
  assertEquals(del.rule, "no-del");
  // review-would-hold write: allowed with "would have held"
  const held = decide(post("api.example", "/y"), pol);
  assertEquals(held.verdict, "allow");
  assert(held.reason.includes("would have held"));
  // a plain read keeps its normal reason (shadow agreed)
  const read = decide(get("api.github.com", "/user"), pol);
  assertEquals(read.verdict, "allow");
  assertEquals(read.reason, "read action GET /user");
});

Deno.test("yolo: classification and facets still land on the decision", () => {
  const pol = policy({ mode: "yolo", k8sHosts: new Set(["k8s.example"]) });
  const d = decide(
    { host: "k8s.example", method: "POST", path: "/api/v1/namespaces/prod/pods/web-1/exec" },
    pol,
  );
  assertEquals(d.verdict, "allow");
  assertEquals(d.action.service, "k8s");
  assertEquals(d.action.facet?.fields.resource, "pods/exec");
});
