import { assertEquals, assertThrows } from "jsr:@std/assert@1";
import {
  activationsFor,
  expandEnv,
  loadRegistry,
  removeServer,
  saveRegistry,
  setActivation,
  upsertServer,
} from "./config.ts";

function withMcpDir(fn: () => void) {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  try {
    fn();
  } finally {
    Deno.env.delete("BOUGH_MCP_DIR");
  }
}

Deno.test("registry: empty when absent, round-trips, rejects bad shapes", () => {
  withMcpDir(() => {
    assertEquals(loadRegistry(), { servers: {} });
    saveRegistry({
      servers: { echo: { command: "deno", args: ["run", "srv.ts"] } },
    });
    const reg = loadRegistry();
    assertEquals(Object.keys(reg.servers).sort(), ["echo"]);
    assertEquals(reg.servers.echo.command, "deno");
    // exactly one of command|url
    assertThrows(() => saveRegistry({ servers: { bad: {} } }));
    assertThrows(() => saveRegistry({ servers: { bad: { command: "x", url: "https://y" } } }));
    // names are lowercase slugs
    assertThrows(() => saveRegistry({ servers: { "Bad Name": { command: "x" } } }));
  });
});

Deno.test("upsertServer adds/replaces one entry without touching siblings; removeServer deletes", () => {
  withMcpDir(() => {
    saveRegistry({ servers: { exa: { command: "npx", args: ["exa-mcp"] } } });
    upsertServer("echo", { command: "deno", args: ["run", "srv.ts"] });
    assertEquals(Object.keys(loadRegistry().servers).sort(), ["echo", "exa"]);
    assertEquals(loadRegistry().servers.exa.args, ["exa-mcp"]); // sibling untouched

    upsertServer("echo", { url: "https://mcp.example.com/mcp" });
    assertEquals(loadRegistry().servers.echo.url, "https://mcp.example.com/mcp");

    assertThrows(() => upsertServer("Bad Name", { command: "x" }));
    assertThrows(() => upsertServer("bad", {})); // one of command|url
    assertThrows(() => upsertServer("bad", { command: "x", url: "https://y" }));

    assertEquals(removeServer("echo"), true);
    assertEquals(removeServer("echo"), false);
    assertEquals(Object.keys(loadRegistry().servers).sort(), ["exa"]);
  });
});

Deno.test("expandEnv substitutes ${VAR} and throws on a missing one", () => {
  Deno.env.set("BOUGH_TEST_SECRET", "s3cr3t");
  try {
    assertEquals(
      expandEnv({ TOKEN: "${BOUGH_TEST_SECRET}", PLAIN: "as-is" }),
      { TOKEN: "s3cr3t", PLAIN: "as-is" },
    );
    assertThrows(
      () => expandEnv({ TOKEN: "${BOUGH_TEST_MISSING_VAR}" }),
      Error,
      "BOUGH_TEST_MISSING_VAR",
    );
  } finally {
    Deno.env.delete("BOUGH_TEST_SECRET");
  }
});

Deno.test("activations: per-session + global scopes, TTL lapse fails closed", () => {
  withMcpDir(() => {
    assertEquals(activationsFor("s1"), []);
    setActivation("s1", "echo", true);
    setActivation(undefined, "linear", true); // global
    assertEquals(activationsFor("s1").sort(), ["echo", "linear"]);
    assertEquals(activationsFor("s2"), ["linear"]);

    // an expired activation is filtered out
    setActivation("s1", "echo", true, new Date(Date.now() - 1000).toISOString());
    assertEquals(activationsFor("s1"), ["linear"]);

    // re-enable replaces the lapsed one; disable removes
    setActivation("s1", "echo", true);
    assertEquals(activationsFor("s1").sort(), ["echo", "linear"]);
    setActivation("s1", "echo", false);
    setActivation(undefined, "linear", false);
    assertEquals(activationsFor("s1"), []);
  });
});
