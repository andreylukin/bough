/**
 * Stylesheet for the artifact spec viewer. One string, inlined into the wrapper
 * page by bundle.ts; class names are consumed by registry.tsx components.
 *
 * Design language: a technical document from a terminal-native agent. Structure is
 * labeled in monospace (section eyebrows, table headers, stat labels) — the one
 * deliberate signature — while content stays in a quiet system-ui with a capped
 * reading measure. Colors follow the dataviz reference palette (validated
 * light/dark pairs): series blue #2a78d6/#3987e5, status good #0ca30c / warn
 * #fab219 / critical #d03b3b, surfaces #fcfcfb/#1a1a19. Values and labels wear
 * text tokens, never the series color. --border outlines boxes; --rule is the
 * fainter line inside tables so rows recede behind the data.
 */
export const VIEWER_CSS = `
:root {
  color-scheme: light;
  --surface: #fcfcfb;
  --surface-2: #f0efec;
  --border: #dedcd6;
  --rule: #eae8e2;
  --text: #0b0b0b;
  --text-2: #52514e;
  --accent: #2a78d6;
  --series-1: #2a78d6;
  --good: #0ca30c;
  --warn: #fab219;
  --serious: #ec835a;
  --critical: #d03b3b;
  --mono: ui-monospace, "SF Mono", Menlo, monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    color-scheme: dark;
    --surface: #1a1a19;
    --surface-2: #232322;
    --border: #3a3936;
    --rule: #2b2b29;
    --text: #f4f3ef;
    --text-2: #c3c2b7;
    --accent: #3987e5;
    --series-1: #3987e5;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--surface);
  color: var(--text);
  font: 14px/1.55 system-ui, sans-serif;
}
#root { max-width: 980px; margin: 0 auto; padding: 32px 28px 8px; }
.b-page > header {
  margin-bottom: 26px;
  padding-bottom: 14px;
  border-bottom: 1px solid var(--border);
}
.b-page h1 { font-size: 21px; font-weight: 650; letter-spacing: -0.01em; margin: 0; }
.b-page .b-subtitle { color: var(--text-2); font: 13px var(--mono); margin: 5px 0 0; }
.b-section { margin: 30px 0; }
.b-section > h2 {
  font: 600 11.5px var(--mono);
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--text-2);
  margin: 0 0 10px;
}
.b-section .b-hint { color: var(--text-2); font-size: 12.5px; margin: -6px 0 10px; }
.b-columns { display: grid; gap: 12px; margin: 12px 0; }
@media (max-width: 640px) { .b-columns { grid-template-columns: 1fr !important; } }
.b-stat { padding: 10px 12px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); }
.b-stat .b-label {
  font: 500 10.5px var(--mono);
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--text-2);
}
.b-stat .b-value { font-size: 24px; font-weight: 650; font-variant-numeric: tabular-nums; margin-top: 2px; }
.b-stat .b-delta { font-size: 12px; margin-top: 1px; }
.b-stat .b-delta.good { color: var(--good); }
.b-stat .b-delta.bad { color: var(--critical); }
.b-stat .b-delta.neutral { color: var(--text-2); }
.b-text { margin: 10px 0; max-width: 70ch; }
.b-text.muted { color: var(--text-2); }
.b-text.mono { font-family: var(--mono); font-size: 13px; }
.b-callout {
  margin: 14px 0;
  padding: 9px 13px;
  max-width: 70ch;
  border-left: 3px solid;
  border-radius: 0 4px 4px 0;
  background: var(--surface-2);
}
.b-callout.info { border-color: var(--accent); }
.b-callout.success { border-color: var(--good); }
.b-callout.warn { border-color: var(--warn); }
.b-callout.error { border-color: var(--critical); }
.b-callout .b-title { font-weight: 600; margin-right: 6px; }
.b-badge {
  display: inline-block;
  padding: 0 7px;
  border: 1px solid var(--border);
  border-radius: 9px;
  font: 11.5px var(--mono);
  line-height: 19px;
}
.b-badge.success { border-color: var(--good); color: var(--good); }
.b-badge.warn { border-color: var(--warn); color: var(--text); }
.b-badge.error { border-color: var(--critical); color: var(--critical); }
.b-badge.info { border-color: var(--accent); color: var(--accent); }
.b-tablewrap { overflow-x: auto; margin: 12px 0; }
.b-table { border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums; }
.b-table caption { caption-side: bottom; color: var(--text-2); font-size: 12px; text-align: left; padding-top: 6px; }
.b-table th {
  font: 600 10.5px var(--mono);
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--text-2);
  text-align: left;
  cursor: pointer;
  user-select: none;
  white-space: nowrap;
  border-bottom: 1px solid var(--border);
}
.b-table th, .b-table td { padding: 6px 22px 6px 0; }
.b-table th:last-child, .b-table td:last-child { padding-right: 0; }
.b-table td { border-bottom: 1px solid var(--rule); vertical-align: top; }
.b-table th.right, .b-table td.right { text-align: right; }
.b-table tbody tr:hover { background: var(--surface-2); }
.b-kv { display: grid; grid-template-columns: max-content 1fr; gap: 3px 18px; margin: 10px 0; }
.b-kv .b-k { color: var(--text-2); font: 12.5px/1.6 var(--mono); }
.b-list { margin: 10px 0; padding-left: 22px; max-width: 70ch; }
.b-list li { margin: 3px 0; }
.b-list li::marker { color: var(--text-2); }
.b-code { margin: 12px 0; border: 1px solid var(--border); border-radius: 4px; overflow: hidden; }
.b-code .b-codetitle {
  padding: 4px 10px;
  font: 11px var(--mono);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--text-2);
  background: var(--surface-2);
  border-bottom: 1px solid var(--border);
}
.b-code pre { margin: 0; padding: 9px 11px; overflow-x: auto; font: 12.5px/1.5 var(--mono); }
.b-chart { margin: 14px 0; }
.b-chart .b-title {
  font: 600 10.5px var(--mono);
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: var(--text-2);
  margin-bottom: 7px;
}
.b-chart .b-row { display: grid; grid-template-columns: minmax(60px, max-content) 1fr max-content; gap: 0 12px; align-items: center; padding: 2px 0; }
.b-chart .b-row:hover { background: var(--surface-2); }
.b-chart .b-barlabel { font-size: 12.5px; color: var(--text-2); white-space: nowrap; }
.b-chart .b-bar { height: 13px; background: var(--series-1); border-radius: 0 4px 4px 0; min-width: 1px; }
.b-chart .b-barvalue { font: 12px var(--mono); font-variant-numeric: tabular-nums; }
a { color: var(--accent); }
hr { border: none; border-top: 1px solid var(--border); margin: 20px 0; }
img { max-width: 100%; }
footer.b-foot {
  max-width: 980px;
  margin: 30px auto 0;
  padding: 10px 28px 16px;
  color: var(--text-2);
  font: 11.5px var(--mono);
  border-top: 1px solid var(--border);
  display: flex;
  justify-content: space-between;
  gap: 12px;
}
footer.b-foot a { color: var(--text-2); }
`;
