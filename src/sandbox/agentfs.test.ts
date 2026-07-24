import { assert, assertEquals } from "jsr:@std/assert@1";
import { execCommand, sandboxAgentfs, stripBanner } from "./agentfs.ts";

Deno.test("stripBanner drops the trailing session banner, keeps command output", () => {
  const banner = "Welcome to AgentFS!\n\nThe following directories are writable:\n" +
    "  - /some/dir (copy-on-write)\n  - /tmp\n\nSession: abc\n" +
    "To see what changed:\n  agentfs diff abc\n";
  // The newline separating the output from the banner block is consumed with it.
  assertEquals(stripBanner(`real output\n${banner}`), "real output");
  // No banner present → untouched.
  assertEquals(stripBanner("just output\n"), "just output\n");
  // A line merely containing the words is not the banner signature.
  assertEquals(
    stripBanner("Welcome to AgentFS! is my slogan\n"),
    "Welcome to AgentFS! is my slogan\n",
  );
});

Deno.test("execCommand wraps argv to run under a session and suppress the banner", () => {
  const argv = execCommand("sess1", ["/bin/sh", "-c", "echo hi"]);
  assertEquals(argv[0], "/bin/sh");
  assertEquals(argv[1], "-c");
  // The session id is threaded through, the inner command's stderr is merged,
  // and agentfs's own stderr (the banner) is dropped.
  assert(argv.includes("sess1"));
  assert(argv.some((a) => a.includes("run --session")));
  assert(argv.some((a) => a.includes("2>/dev/null")));
  assert(argv.some((a) => a.includes("exec 2>&1")));
  // The command itself travels as a positional (no quoting/escaping), so it is
  // present verbatim as its own argv element.
  assert(argv.includes("echo hi"));
});

Deno.test("sandboxAgentfs is on by default, off only when explicitly disabled", () => {
  const prior = Deno.env.get("BOUGH_SANDBOX_AGENTFS");
  try {
    Deno.env.delete("BOUGH_SANDBOX_AGENTFS");
    assertEquals(sandboxAgentfs(), true);
    Deno.env.set("BOUGH_SANDBOX_AGENTFS", "1");
    assertEquals(sandboxAgentfs(), true);
    Deno.env.set("BOUGH_SANDBOX_AGENTFS", "0");
    assertEquals(sandboxAgentfs(), false);
  } finally {
    if (prior === undefined) Deno.env.delete("BOUGH_SANDBOX_AGENTFS");
    else Deno.env.set("BOUGH_SANDBOX_AGENTFS", prior);
  }
});
