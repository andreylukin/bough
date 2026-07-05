import { assert, assertEquals, assertThrows } from "jsr:@std/assert@1";
import { compile, ExprError } from "./expr.ts";

const env = {
  http: {
    method: "POST",
    path: "/graphql",
    headers: { "content-type": "application/json" },
    body: '{"query":"mutation { x }"}',
    body_json: { query: "mutation { x }" },
  },
  k8s: { verb: "get", resource: "pods/exec", namespace: "prod", name: "web-1" },
  action: { service: "github", verb: "graphql:mutation", kind: "write" },
};

function evalTo(src: string, expected: boolean) {
  assertEquals(compile(src).test(env), expected, src);
}

Deno.test("expr: equality and boolean composition", () => {
  evalTo("http.method == 'POST'", true);
  evalTo("http.method != 'POST'", false);
  evalTo("http.method == 'POST' && http.path == '/graphql'", true);
  evalTo("http.method == 'GET' || http.path == '/graphql'", true);
  evalTo("!(http.method == 'GET')", true);
});

Deno.test("expr: in on lists and maps", () => {
  evalTo("http.method in ['GET', 'HEAD']", false);
  evalTo("http.method in ['POST', 'PUT']", true);
  evalTo("k8s.resource in ['pods/exec', 'pods/attach']", true);
  evalTo("'content-type' in http.headers", true);
  evalTo("'authorization' in http.headers", false);
});

Deno.test("expr: string methods", () => {
  evalTo("http.body_json.query.startsWith('mutation')", true);
  evalTo("k8s.resource.endsWith('/exec')", true);
  evalTo("http.body.contains('mutation')", true);
  evalTo("k8s.name.matches('^web-[0-9]+$')", true);
});

Deno.test("expr: indexing maps", () => {
  evalTo("http.headers['content-type'].contains('json')", true);
});

Deno.test("expr: has() guards optional fields", () => {
  evalTo("has(http.body_json.query)", true);
  evalTo("has(http.body_json.archived)", false);
  evalTo("has(http.body_json.archived) && http.body_json.archived == true", false);
});

Deno.test("expr: has() is false (not an error) when the path can't resolve", () => {
  // absent facet root — a k8s rule evaluated against a plain-http request
  evalTo("has(missing_facet.verb)", false);
  evalTo("has(missing_facet.verb) && missing_facet.verb == 'get'", false);
  // body_json on a non-JSON body
  const raw = {
    http: {
      get body_json(): unknown {
        throw new ExprError("not JSON");
      },
    },
  };
  assertEquals(compile("has(http.body_json.query)").test(raw as never), false);
});

Deno.test("expr: numbers and literals", () => {
  evalTo("1 == 1", true);
  evalTo("true", true);
  evalTo("false || true", true);
});

Deno.test("expr: compile errors on malformed input", () => {
  assertThrows(() => compile("http.method =="), ExprError);
  assertThrows(() => compile("http.method == 'POST"), ExprError);
  assertThrows(() => compile("(http.method == 'POST'"), ExprError);
  assertThrows(() => compile("http.method == 'a' extra"), ExprError);
  assertThrows(() => compile("http.body.explode('x')"), ExprError);
  assertThrows(() => compile("has(http)"), ExprError);
});

Deno.test("expr: evaluation errors throw (fail closed in decide)", () => {
  // unknown root identifier
  assertThrows(() => compile("nope.field == 1").test(env), ExprError);
  // selecting a field the payload doesn't carry
  assertThrows(() => compile("http.body_json.missing == 1").test(env), ExprError);
  // method on a non-string
  assertThrows(() => compile("http.headers.startsWith('x')").test(env), ExprError);
  // non-boolean result
  assertThrows(() => compile("http.method").test(env), ExprError);
  // non-boolean operand to &&
  assertThrows(() => compile("http.method && true").test(env), ExprError);
});

Deno.test("expr: && short-circuits so guarded fields stay untouched", () => {
  const lazyEnv = {
    http: {
      path: "/other",
      get body_json(): Record<string, unknown> {
        throw new ExprError("not JSON");
      },
    },
  };
  const expr = compile("http.path == '/graphql' && http.body_json.query.startsWith('mutation')");
  assertEquals(expr.test(lazyEnv), false);
  // and when the left side matches, the error propagates
  const expr2 = compile("http.path == '/other' && http.body_json.query.startsWith('mutation')");
  assertThrows(() => expr2.test(lazyEnv), ExprError);
});

Deno.test("expr: string escapes", () => {
  assert(compile("'it\\'s' == 'it\\'s'").test({}));
});
