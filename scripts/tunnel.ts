/**
 * Expose the local bough server on a temporary public HTTPS URL via a Cloudflare
 * quick tunnel (needs `cloudflared`, e.g. `brew install cloudflared`). For using bough
 * from a phone while the server runs on this machine.
 *
 *   BOUGH_PASSWORD=... deno task dev     # terminal 1 — server, with auth on
 *   deno task tunnel                     # terminal 2 — prints the public URL
 *
 * Refuses to open the tunnel if the server answers without auth (no BOUGH_PASSWORD),
 * since a quick-tunnel URL is public to anyone who has it. Ctrl-C closes the tunnel;
 * quick tunnels get a fresh random URL each run.
 */

const port = Number(Deno.env.get("BOUGH_PORT") ?? 4321);
const local = `http://127.0.0.1:${port}`;

// Probe: the server must be up, and must reject an unauthenticated API call.
let status: number;
try {
  const res = await fetch(`${local}/sessions`);
  await res.body?.cancel();
  status = res.status;
} catch {
  console.error(`no server at ${local} — start it first: BOUGH_PASSWORD=... deno task dev`);
  Deno.exit(1);
}
if (status !== 401) {
  console.error(
    `refusing to tunnel: ${local} answered ${status} without auth.\n` +
      `Restart the server with a password: BOUGH_PASSWORD=... deno task dev`,
  );
  Deno.exit(1);
}

console.log(`tunneling ${local} — waiting for the public URL...`);
const child = new Deno.Command("cloudflared", {
  args: ["tunnel", "--no-autoupdate", "--url", local],
  stdout: "null",
  stderr: "piped", // cloudflared logs (incl. the URL banner) go to stderr
}).spawn();

const decoder = new TextDecoder();
let buffer = "";
let announced = false;
for await (const chunk of child.stderr) {
  buffer += decoder.decode(chunk, { stream: true });
  const match = buffer.match(/https:\/\/[a-z0-9-]+\.trycloudflare\.com/);
  if (match && !announced) {
    announced = true;
    console.log(`\n  public URL: ${match[0]}\n\nOpen it on your phone; sign in with BOUGH_PASSWORD. Ctrl-C to stop.`);
  }
}
const { code } = await child.status;
Deno.exit(code);
