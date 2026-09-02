// bough graphiti: remember the turn. Installed by `bough graphiti install`;
// runs on the `stop` hook with {input, reply}. The add is queued in the
// background so the turn does not wait on extraction.
var BOUGH = "__BOUGH__";
var input = event.input, reply = event.reply;
if (!input) return;
var i = input.indexOf("\n\n[memory]\n");
if (i >= 0) input = input.slice(0, i);
if (input.charAt(0) === "/" || input.charAt(0) === "!") return;
var body = "User: " + input + "\n\nAssistant: " + (reply || "");
if (body.length > 12000) body = body.slice(0, 12000) + "\n[truncated]";
var args = JSON.stringify({
  name: "bough turn " + new Date().toISOString(),
  episode_body: body,
  source: "message",
  source_description: "bough session turn"
});
function sq(s) { return "'" + s.replace(/'/g, "'\\''") + "'"; }
try {
  tools.bash("nohup " + sq(BOUGH) + " mcp call graphiti/add_memory " + sq(args) + " >/dev/null 2>&1 &");
} catch (e) {}
