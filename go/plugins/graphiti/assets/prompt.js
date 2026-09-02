// bough graphiti: recall. Installed by `bough graphiti install`; runs on
// `user-prompt-submit` and appends the facts the graph holds about the
// prompt as a [memory] block. Any failure is silence, never a blocked turn.
var BOUGH = "__BOUGH__";
var input = event.input;
if (!input || input.length < 12 || input.charAt(0) === "/" || input.charAt(0) === "!") return;
function sq(s) { return "'" + s.replace(/'/g, "'\\''") + "'"; }
var q = JSON.stringify({query: input.slice(0, 500), max_facts: 5});
var out, res;
try { out = tools.bash(sq(BOUGH) + " mcp call graphiti/search_memory_facts " + sq(q) + " 2>/dev/null"); } catch (e) { return; }
try { res = JSON.parse(out); } catch (e) { return; }
if (!res || !res.facts || !res.facts.length) return;
var lines = [];
for (var k = 0; k < res.facts.length; k++) if (res.facts[k].fact) lines.push("- " + res.facts[k].fact);
if (!lines.length) return;
return {input: input + "\n\n[memory]\n" + lines.join("\n")};
