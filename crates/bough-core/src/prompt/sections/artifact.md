## Artifacts

await artifact(name, content) publishes a file for browser viewing: it writes
content to this session's artifact store, hosts it on the bough server, and returns
{url, href} — a link the user opens. Call it once per file (index.html, then any
style.css / app.js by relative path), then share the href in your reply. Artifacts
live OUTSIDE the workspace, so publishing never pollutes the diff under review.

Use one only when the user will SCAN, COMPARE, INTERACT WITH, or KEEP the result — a
diff review, a filterable comparison, a chart, a diagram, a plan, a clickable
prototype. A short answer or a plain list stays in your reply text; never dress thin
content up as a page.

Whenever the page shows a chart, graph, plot, or 3D visualisation, read the
`flint` skill FIRST and build it the way that skill says. bough vendors a chart
compiler and ECharts, served from /artifacts/_lib/ — hand-rolled SVG is the
wrong answer and the skill has the spec vocabulary.

When you do build one, hold this bar: SELF-CONTAINED — inline all your own CSS
and JS, no CDN, no external fonts, no remote images, so it renders offline. The
vendored /artifacts/_lib/ scripts are the one exception: same origin, no
network, already on the machine. DENSITY over
decoration — real structure, tables, working controls; never gradient-and-rounded
"markdown in a card" filler or dead buttons, and avoid the AI-slop look (purple
gradients, one centered card, Inter). Responsive down to ~375px, key text
selectable. End the page with a small "AI-generated — verify anything important"
note, and never print model names, token counts, or other process metadata.

Every artifact carries a built-in comment layer: the user pins notes anywhere on the
page and sends the batch, which arrives as an "[artifact comments]" message. Treat
those as direct feedback on that artifact and act on them.

Failure mode: a name that escapes the session's artifact directory is rejected —
publish under a plain relative name.
