import { assertEquals, assertRejects } from "jsr:@std/assert@1";
import { McpStdioClient } from "./client.ts";

// Spawns a real child process; self-skips without --allow-run (test task grants it).
async function canRun(): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run" })).state === "granted";
}

const FIXTURE = new URL("./testdata/echo_server.ts", import.meta.url).pathname;

function connectFixture(): Promise<McpStdioClient> {
  return McpStdioClient.connect({
    argv: [Deno.execPath(), "run", "--quiet", "--no-config", FIXTURE],
    env: {
      PATH: Deno.env.get("PATH") ?? "",
      HOME: Deno.env.get("HOME") ?? "/tmp",
    },
  });
}

Deno.test("stdio client: handshake, paginated tools/list, tools/call, close", async () => {
  if (!(await canRun())) return;
  const client = await connectFixture();
  try {
    const tools = await client.listTools();
    assertEquals(tools.map((t) => t.name), ["echo", "scream", "boom"]); // both pages
    assertEquals(tools[0].annotations?.readOnlyHint, true);

    const res = await client.callTool("echo", { text: "hi" });
    assertEquals(res.structuredContent, { echoed: "hi" });
    assertEquals(res.content?.[0]?.text, "hi");
    assertEquals(res.isError ?? false, false);

    const boom = await client.callTool("boom", {});
    assertEquals(boom.isError, true);
  } finally {
    await client.close();
  }
  assertEquals(client.alive, false);
});

Deno.test("stdio client: requests after close reject", async () => {
  if (!(await canRun())) return;
  const client = await connectFixture();
  await client.close();
  await assertRejects(() => client.listTools(), Error, "down");
});

Deno.test("stdio client: a dead server fails the connect handshake", async () => {
  if (!(await canRun())) return;
  await assertRejects(() =>
    McpStdioClient.connect({
      argv: ["/usr/bin/false"],
      env: { PATH: Deno.env.get("PATH") ?? "" },
    })
  );
});
